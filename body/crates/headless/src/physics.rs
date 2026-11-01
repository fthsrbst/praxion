//! Client physics for one local player: azalea's per-tick step, lifted.
//!
//! The body plays the role of the game client, so it may read the raw store for
//! collision and fluids (docs/decisions.md 2026-11-01). Decision-making layers
//! still see the world only through perception; this module is below them.
//!
//! # What is lifted, and from where
//!
//! The algorithms are `azalea-physics`'s, ported without its bevy systems
//! (`docs/research/demo0-azalea-headless.md` §2, pinned commit `8041a14`):
//!
//! | Step | Source |
//! |---|---|
//! | input -> acceleration, sprint start/stop | `azalea-client/src/plugins/movement.rs` `tick_controls`, `local_player_ai_step`, `modify_input` |
//! | water/lava state and fluid pushing | `azalea-physics/src/fluids.rs` |
//! | jump gating and jump velocity | `azalea-physics/src/lib.rs` `ai_step`, `jump_from_ground` |
//! | friction, gravity, travel in air and fluid | `azalea-physics/src/travel.rs` |
//! | collision, step-up, sneak edge guard, fall distance | `azalea-physics/src/collision/mod.rs` |
//!
//! The collision *shapes* stay in `azalea-physics` (`BlockWithShape`): its
//! generated table is 16,730 lines and copying it would be a fork, not a lift.
//! Everything else here is our glue: reading blocks from [`Blocks`] instead of
//! `azalea_world::World`, and one function per azalea system, chained in the
//! same order (`azalea-physics/src/lib.rs:39-52`, `movement.rs:57-72`):
//!
//! ```text
//! pose/dimensions -> controls -> sprint -> acceleration
//!   -> fluids -> old position -> ai_step -> travel
//! ```
//!
//! # Deliberate differences from azalea
//!
//! - Entity collisions (other players, mobs) and the world border are not
//!   implemented: the raw store has no collidable-entity index yet, and Demo-0's
//!   body is alone. Their queries in azalea return no boxes for the same reason
//!   (`travel.rs:320-324` is a `TODO` for the border).
//! - `apply_effects_from_blocks` only handles bubble columns; it is not ported.
//! - The lava push factor is the non-ultrawarm one: the raw store's dimension
//!   data does not expose `ultrawarm` yet (backlog).
//! - Crouching lags the sneak key by one tick, as in azalea: its `Crouching`
//!   comes from `LastSentInput`, the input of the previous tick. Ported tests
//!   below depend on this for their exact numbers.
//! - Sprint stops when the sprint key is released; azalea only stops on its
//!   `should_stop_sprinting` conditions (its walk events are the missing piece).
//!
//! # Using it
//!
//! ```text
//! let mut body = Body::new(spawn_position);
//! body.step(&held, &world);          // one client tick
//! body.position(); body.on_ground(); // what the packet layer sends
//! ```

use azalea_block::{
    BlockState, BlockTrait,
    fluid_state::{FluidKind, FluidState},
};
use azalea_core::{
    aabb::Aabb,
    cursor3d::{Cursor3d, CursorIterationType},
    direction::{Axis, Direction},
    math::{self, EPSILON},
    position::{BlockPos, ChunkPos, Vec2, Vec3},
};
use azalea_entity::{
    ActiveEffects, Attributes, LookDirection, Physics, PlayerAbilities, Pose, Position,
    dimensions::{EntityDimensions, calculate_dimensions},
};
use azalea_inventory::{ItemStack, components::Tool};
use azalea_physics::{
    collision::{BLOCK_SHAPE, BlockWithShape, Shapes, VoxelShape, legacy_blocks_motion},
    get_block_pos_below_that_affects_movement,
    travel::fluid_jump_threshold,
};
use azalea_protocol::common::movements::{PositionMoveRotation, RelativeMovements};
use azalea_registry::builtin::EntityKind;

use crate::frame::Held;

/// Gravity per tick (vanilla `0.08`).
const GRAVITY: f64 = 0.08;
/// Vertical air drag after the gravity step.
const VERTICAL_DRAG: f64 = 0.9800000190734863;
/// The step height used by vanilla's collision step-up.
const MAX_UP_STEP: f64 = 0.6;
/// Below this horizontal speed the player is considered standing still.
const MIN_PLAYER_SPEED_SQUARED: f64 = 9.0e-6;
/// Below this absolute vertical speed, velocity.y is zeroed.
const MIN_VERTICAL_SPEED: f64 = 0.003;

/// Read-only block access, the only thing physics needs from the world.
///
/// `None` means "no data": past the world edge, or a chunk that is not loaded.
/// Both read as air, exactly like `azalea_world`'s `unwrap_or_default()`.
pub trait Blocks {
    fn block(&self, pos: BlockPos) -> Option<BlockState>;

    /// Whether the chunks around `position` are loaded, so physics may run.
    ///
    /// Vanilla freezes a player whose chunk is not loaded: no movement and no
    /// gravity. The default is `true`, for fixtures that provide every block
    /// through [`Blocks::block`]. A world with no dimension at all is "not
    /// playing" rather than "an unloaded chunk", so it keeps the old behaviour
    /// (read as air) and the physics tests can use an empty [`praxion_world::World`].
    fn ready(&self, _position: Vec3) -> bool {
        true
    }
}

impl Blocks for praxion_world::World {
    fn block(&self, pos: BlockPos) -> Option<BlockState> {
        self.block_state(pos)
    }

    fn ready(&self, position: Vec3) -> bool {
        if !self.status().is_valid() || self.dimension().is_none() {
            return true;
        }
        // Vanilla freezes a player whose own chunk is not loaded. The live
        // session additionally refuses to step until the 3x3 chunks around the
        // player are there (`play.rs::world_ready`); this check is the minimum
        // that keeps a direct `Body::step` caller from falling through a gap.
        self.has_chunk(ChunkPos::from(&position))
    }
}

/// One local player: position, look, physics state and the input of the last
/// tick.
///
/// Create it at the server's teleport position, then call [`Body::step`] once per
/// client tick. The server moving us (`player_position`) goes through
/// [`Body::teleport`], which also bumps [`Body::generation`].
#[derive(Clone, Debug)]
pub struct Body {
    position: Position,
    look: LookDirection,
    physics: Physics,
    dimensions: EntityDimensions,
    pose: Pose,
    attributes: Attributes,
    abilities: PlayerAbilities,
    active_effects: ActiveEffects,
    sprinting: bool,
    /// The crouching pose, one tick behind the sneak key (see the module docs).
    crouching: bool,
    /// The sneak key of the current tick, used by the edge guard and water descent.
    trying_to_crouch: bool,
    last_input: Held,
    generation: u32,
}

impl Body {
    /// A player at `position`, standing, not moving.
    pub fn new(position: Vec3) -> Self {
        let pose = Pose::Standing;
        let dimensions = calculate_dimensions(EntityKind::Player, pose);
        Self {
            position: Position::new(position),
            look: LookDirection::default(),
            physics: Physics::new(&dimensions, position),
            dimensions,
            pose,
            attributes: Attributes::new(EntityKind::Player),
            abilities: PlayerAbilities::default(),
            active_effects: ActiveEffects::default(),
            sprinting: false,
            crouching: false,
            trying_to_crouch: false,
            last_input: Held::NONE,
            generation: 0,
        }
    }

    pub fn position(&self) -> Vec3 {
        *self.position
    }

    pub fn look(&self) -> LookDirection {
        self.look
    }

    pub fn set_look(&mut self, look: LookDirection) {
        self.look = look;
    }

    pub fn on_ground(&self) -> bool {
        self.physics.on_ground()
    }

    pub fn horizontal_collision(&self) -> bool {
        self.physics.horizontal_collision
    }

    pub fn sprinting(&self) -> bool {
        self.sprinting
    }

    /// The body state frames are built against. Bumped whenever the server moves
    /// us or the world under us is replaced.
    pub fn generation(&self) -> u32 {
        self.generation
    }

    /// Move to a new generation: every frame built before this is stale.
    pub fn bump_generation(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    /// Apply a `player_position` (teleport) packet exactly as azalea's packet
    /// handler does: relative flags, then the position becomes the old position.
    pub fn teleport(&mut self, change: &PositionMoveRotation, relative: &RelativeMovements) {
        relative.apply(change, &mut self.position, &mut self.look, &mut self.physics);
        self.physics.set_old_pos(self.position);
        self.physics.bounding_box = self.dimensions.make_bounding_box(*self.position);
        self.bump_generation();
    }

    /// Stop where we are and move to a new generation (respawn).
    pub fn respawn(&mut self) {
        self.physics.velocity = Vec3::ZERO;
        self.physics.set_old_pos(self.position);
        self.physics.set_on_ground(false);
        self.sprinting = false;
        self.bump_generation();
    }

    /// One client tick: the frame's held controls, then azalea's physics order.
    pub fn step(&mut self, held: &Held, world: &dyn Blocks) {
        // Vanilla freezes a player whose chunk is not loaded: no movement, no
        // gravity (review m1). The input is remembered so the first tick after
        // the chunk arrives sees the held keys.
        if !world.ready(self.position()) {
            self.last_input = *held;
            return;
        }
        // Update, before GameTick: pose, dimensions and bounding box from the
        // (one tick old) crouching state.
        self.crouching = self.last_input.sneak;
        self.trying_to_crouch = held.sneak;
        self.pose = if self.crouching {
            Pose::Crouching
        } else {
            Pose::Standing
        };
        self.dimensions = calculate_dimensions(EntityKind::Player, self.pose);
        self.physics.bounding_box = self.dimensions.make_bounding_box(*self.position);

        // tick_controls + local_player_ai_step.
        let mut move_vector = tick_controls(held);
        let has_enough_impulse = move_vector.y > 0.8;
        let can_start_sprinting = !self.sprinting
            && has_enough_impulse
            && !self.crouching
            && !self.physics.is_in_water();
        if held.sprint && can_start_sprinting {
            self.set_sprinting(true);
        }
        if self.sprinting {
            let should_stop = !held.sprint
                || !has_enough_impulse
                || (self.physics.horizontal_collision && !self.physics.minor_horizontal_collision)
                || self.physics.is_in_water();
            if should_stop {
                self.set_sprinting(false);
            }
        }
        move_vector = modify_input(move_vector, self.crouching, &self.attributes);
        self.physics.x_acceleration = move_vector.x;
        self.physics.z_acceleration = move_vector.y;

        // fluids::update_in_water_state_and_do_fluid_pushing
        self.update_fluids(world);
        // update_old_position
        self.physics.set_old_pos(self.position);
        // ai_step
        self.ai_step(held, world);
        // travel::travel
        self.travel(held, world);
        // apply_effects_from_blocks is bubble columns only; not ported.

        self.last_input = *held;
    }

    /// Whether the body currently touches water (after the last step).
    pub fn is_in_water(&self) -> bool {
        self.physics.is_in_water()
    }

    /// The eye position the interaction ray starts from.
    pub fn eye_position(&self) -> Vec3 {
        self.position.up(f64::from(self.dimensions.eye_height))
    }

    /// The vanilla 26.3 block interaction range (4.5 blocks).
    pub fn block_interaction_range(&self) -> f64 {
        self.attributes.block_interaction_range.calculate()
    }

    /// The vanilla 26.3 entity interaction range (3.0 blocks).
    pub fn entity_interaction_range(&self) -> f64 {
        self.attributes.entity_interaction_range.calculate()
    }

    /// Ticks between two full-strength attacks (`20 / attack_speed`).
    pub fn attack_cooldown_ticks(&self) -> u32 {
        let speed = self.attributes.attack_speed.calculate();
        if speed <= 0. {
            return 20;
        }
        ((1. / speed) * 20.).ceil() as u32
    }

    /// How much destroy progress one tick adds to `block` with `held`.
    ///
    /// Ported from `azalea-entity/src/mining.rs:17-129` (`get_mine_progress`,
    /// `destroy_speed`, `base_destroy_speed`, `has_correct_tool_for_drops`).
    /// Haste and mining fatigue come from [`ActiveEffects`], efficiency and
    /// block-break speed from the attributes. The eye-in-water aqua-affinity
    /// adjustment is a TODO in azalea too (`mining.rs:103-109`).
    pub fn mine_progress(&self, block: BlockState, held: &ItemStack) -> f32 {
        let block = Box::<dyn BlockTrait>::from(block);
        let destroy_time = block.behavior().destroy_time;
        if destroy_time == -1. {
            return 0.;
        }
        let divisor = if has_correct_tool_for_drops(block.as_ref(), held) {
            30.
        } else {
            100.
        };
        let mut speed = base_destroy_speed(block.as_block_kind(), held);
        if speed > 1. {
            speed += self.attributes.mining_efficiency.calculate() as f32;
        }
        if let Some(amplifier) = self.active_effects.get_dig_speed_amplifier() {
            speed *= 1. + (amplifier + 1) as f32 * 0.2;
        }
        if let Some(level) = self
            .active_effects
            .get_level(azalea_registry::builtin::MobEffect::MiningFatigue)
        {
            let multiplier = match level {
                0 => 0.3,
                1 => 0.09,
                2 => 0.0027,
                _ => 8.1E-4,
            };
            speed *= multiplier;
        }
        speed *= self.attributes.block_break_speed.calculate() as f32;
        if !self.on_ground() {
            speed /= 5.;
        }
        (speed / destroy_time) / divisor
    }

    fn set_sprinting(&mut self, sprinting: bool) {
        if self.sprinting == sprinting {
            return;
        }
        self.sprinting = sprinting;
        let modifier = azalea_entity::attributes::sprinting_modifier();
        if sprinting {
            let _ = self.attributes.movement_speed.try_insert(modifier);
        } else {
            self.attributes.movement_speed.remove(&modifier.id);
        }
    }

    // ------------------------------------------------------------------
    // ai_step and jump (azalea-physics/src/lib.rs:68-187, 350-387)
    // ------------------------------------------------------------------

    fn ai_step(&mut self, held: &Held, world: &dyn Blocks) {
        if self.physics.no_jump_delay > 0 {
            self.physics.no_jump_delay -= 1;
        }

        if self.physics.velocity.horizontal_distance_squared() < MIN_PLAYER_SPEED_SQUARED {
            self.physics.velocity.x = 0.;
            self.physics.velocity.z = 0.;
        }
        if self.physics.velocity.y.abs() < MIN_VERTICAL_SPEED {
            self.physics.velocity.y = 0.;
        }

        if self.trying_to_crouch && self.physics.is_in_water() {
            // go_down_in_water
            self.physics.velocity.y -= 0.04;
        }

        if held.jump {
            let fluid_height = if self.physics.is_in_lava() {
                self.physics.lava_fluid_height
            } else if self.physics.is_in_water() {
                self.physics.water_fluid_height
            } else {
                0.
            };
            let in_water = self.physics.is_in_water() && fluid_height > 0.;
            let threshold = fluid_jump_threshold();
            let on_ground = self.physics.on_ground();

            if !in_water || on_ground && fluid_height <= threshold {
                if !self.physics.is_in_lava() || on_ground && fluid_height <= threshold {
                    if (on_ground || in_water && fluid_height <= threshold)
                        && self.physics.no_jump_delay == 0
                    {
                        self.jump_from_ground(world);
                        self.physics.no_jump_delay = 10;
                    }
                } else {
                    // jump_in_liquid
                    self.physics.velocity.y += 0.04;
                }
            } else {
                // jump_in_liquid
                self.physics.velocity.y += 0.04;
            }
        } else {
            self.physics.no_jump_delay = 0;
        }
    }

    fn jump_from_ground(&mut self, world: &dyn Blocks) {
        let jump_power = 0.42 * block_jump_factor(world, self.position);
        if jump_power <= 1.0e-5 {
            return;
        }

        let old_delta = self.physics.velocity;
        self.physics.velocity = Vec3 {
            x: old_delta.x,
            y: f64::max(jump_power as f64, old_delta.y),
            z: old_delta.z,
        };
        if self.sprinting {
            // Sprint jumping gives extra horizontal velocity.
            let y_rot = self.look.y_rot() * 0.017453292;
            self.physics.velocity += Vec3 {
                x: (-math::sin(y_rot) * 0.2) as f64,
                y: 0.,
                z: (math::cos(y_rot) * 0.2) as f64,
            };
        }
        self.physics.has_impulse = true;
    }

    // ------------------------------------------------------------------
    // fluids (azalea-physics/src/fluids.rs:16-242)
    // ------------------------------------------------------------------

    fn update_fluids(&mut self, world: &dyn Blocks) {
        self.physics.water_fluid_height = 0.;
        self.physics.lava_fluid_height = 0.;

        if self.update_fluid_height_and_do_fluid_pushing(world, FluidKind::Water, 0.014) {
            self.physics.reset_fall_distance();
            self.physics.was_touching_water = true;
            self.physics.clear_fire();
        } else {
            self.physics.was_touching_water = false;
        }

        // Non-ultrawarm dimension: the raw store does not expose the dimension
        // type's `ultrawarm` flag yet.
        let lava_push_factor = 0.0023333333333333335;
        self.update_fluid_height_and_do_fluid_pushing(world, FluidKind::Lava, lava_push_factor);
    }

    fn update_fluid_height_and_do_fluid_pushing(
        &mut self,
        world: &dyn Blocks,
        checking_fluid: FluidKind,
        fluid_push_factor: f64,
    ) -> bool {
        let checking_liquids_aabb = self.physics.bounding_box.deflate_all(0.001);

        let min_x = checking_liquids_aabb.min.x.floor() as i32;
        let min_y = checking_liquids_aabb.min.y.floor() as i32;
        let min_z = checking_liquids_aabb.min.z.floor() as i32;
        let max_x = checking_liquids_aabb.max.x.ceil() as i32;
        let max_y = checking_liquids_aabb.max.y.ceil() as i32;
        let max_z = checking_liquids_aabb.max.z.ceil() as i32;

        let mut min_height_touching = 0.;
        let mut touching_fluid = false;
        let mut additional_player_delta = Vec3::ZERO;
        let mut num_fluids_being_touched = 0;

        for cur_x in min_x..max_x {
            for cur_y in min_y..max_y {
                for cur_z in min_z..max_z {
                    let cur_pos = BlockPos::new(cur_x, cur_y, cur_z);
                    let Some(fluid_at_cur_pos) = block_fluid(world, cur_pos) else {
                        continue;
                    };
                    if fluid_at_cur_pos.kind != checking_fluid {
                        continue;
                    }
                    let fluid_max_y = (cur_y as f32 + fluid_at_cur_pos.height()) as f64;
                    if fluid_max_y < checking_liquids_aabb.min.y {
                        continue;
                    }
                    touching_fluid = true;
                    min_height_touching =
                        f64::max(fluid_max_y - checking_liquids_aabb.min.y, min_height_touching);

                    let mut delta_for_fluid = get_fluid_flow(&fluid_at_cur_pos, world, cur_pos);
                    if min_height_touching < 0.4 {
                        delta_for_fluid *= min_height_touching;
                    }
                    additional_player_delta += delta_for_fluid;
                    num_fluids_being_touched += 1;
                }
            }
        }

        if additional_player_delta.length() > 0. {
            additional_player_delta /= num_fluids_being_touched as f64;

            let player_delta = self.physics.velocity;
            additional_player_delta *= fluid_push_factor;
            const MIN_PUSH: f64 = 0.003;
            const MIN_PUSH_LENGTH: f64 = MIN_PUSH * 1.5;

            if player_delta.x.abs() < MIN_PUSH
                && player_delta.z.abs() < MIN_PUSH
                && additional_player_delta.length() < MIN_PUSH_LENGTH
            {
                additional_player_delta = additional_player_delta.normalize() * MIN_PUSH_LENGTH;
            }

            self.physics.velocity += additional_player_delta;
        }

        match checking_fluid {
            FluidKind::Water => self.physics.water_fluid_height = min_height_touching,
            FluidKind::Lava => self.physics.lava_fluid_height = min_height_touching,
            FluidKind::Empty => unreachable!("Empty is never checked as a fluid"),
        }

        touching_fluid
    }

    // ------------------------------------------------------------------
    // travel (azalea-physics/src/travel.rs:102-252)
    // ------------------------------------------------------------------

    fn travel(&mut self, held: &Held, world: &dyn Blocks) {
        if self.physics.is_in_water() || self.physics.is_in_lava() {
            self.travel_in_fluid(world);
        } else {
            self.travel_in_air(held, world);
        }
    }

    fn travel_in_air(&mut self, held: &Held, world: &dyn Blocks) {
        let block_pos_below = get_block_pos_below_that_affects_movement(self.position);
        let block_below = world.block(block_pos_below).unwrap_or(BlockState::AIR);
        let block_friction = Box::<dyn BlockTrait>::from(block_below).behavior().friction;

        let inertia = if self.physics.on_ground() {
            block_friction * 0.91
        } else {
            0.91
        };

        let mut movement =
            self.handle_relative_friction_and_calculate_movement(held, world, block_friction);

        movement.y -= GRAVITY;

        self.physics.velocity = Vec3 {
            x: movement.x * inertia as f64,
            y: movement.y * VERTICAL_DRAG,
            z: movement.z * inertia as f64,
        };
    }

    fn handle_relative_friction_and_calculate_movement(
        &mut self,
        held: &Held,
        world: &dyn Blocks,
        block_friction: f32,
    ) -> Vec3 {
        let speed =
            get_friction_influenced_speed(&self.physics, &self.attributes, block_friction, self.sprinting);
        let acceleration = Vec3::new(
            self.physics.x_acceleration as f64,
            self.physics.y_acceleration as f64,
            self.physics.z_acceleration as f64,
        );
        azalea_entity::move_relative(&mut self.physics, self.look, speed, acceleration);

        // on_climbable is never set in this pin of azalea (nothing writes the
        // component), so handle_on_climbable is a no-op.
        self.move_colliding(world, self.physics.velocity);

        if self.physics.horizontal_collision || held.jump {
            let block_at_feet: azalea_registry::builtin::BlockKind = world
                .block(BlockPos::from(self.position))
                .unwrap_or_default()
                .into();
            if block_at_feet == azalea_registry::builtin::BlockKind::PowderSnow {
                self.physics.velocity.y = 0.2;
            }
        }

        self.physics.velocity
    }

    fn travel_in_fluid(&mut self, world: &dyn Blocks) {
        let moving_down = self.physics.velocity.y <= 0.;
        let y = self.position.y;

        let acceleration = Vec3::new(
            self.physics.x_acceleration as f64,
            self.physics.y_acceleration as f64,
            self.physics.z_acceleration as f64,
        );

        if self.physics.was_touching_water {
            let mut water_movement_speed = if self.sprinting { 0.9 } else { 0.8 };
            let mut speed = 0.02;
            let mut water_efficiency_modifier =
                self.attributes.water_movement_efficiency.calculate() as f32;
            if !self.physics.on_ground() {
                water_efficiency_modifier *= 0.5;
            }

            if water_efficiency_modifier > 0. {
                water_movement_speed +=
                    (0.54600006 - water_movement_speed) * water_efficiency_modifier;
                speed += (self.attributes.movement_speed.calculate() as f32 - speed)
                    * water_efficiency_modifier;
            }

            azalea_entity::move_relative(&mut self.physics, self.look, speed, acceleration);
            self.move_colliding(world, self.physics.velocity);

            let mut new_velocity = self.physics.velocity;
            new_velocity.x *= water_movement_speed as f64;
            new_velocity.y *= 0.8;
            new_velocity.z *= water_movement_speed as f64;
            self.physics.velocity =
                get_fluid_falling_adjusted_movement(GRAVITY, moving_down, new_velocity, self.sprinting);
        } else {
            azalea_entity::move_relative(&mut self.physics, self.look, 0.02, acceleration);
            self.move_colliding(world, self.physics.velocity);

            if self.physics.lava_fluid_height <= fluid_jump_threshold() {
                self.physics.velocity.x *= 0.5;
                self.physics.velocity.y *= 0.8;
                self.physics.velocity.z *= 0.5;
                self.physics.velocity = get_fluid_falling_adjusted_movement(
                    GRAVITY,
                    moving_down,
                    self.physics.velocity,
                    self.sprinting,
                );
            } else {
                self.physics.velocity *= 0.5;
            }

            self.physics.velocity.y -= GRAVITY / 4.0;
        }

        let velocity = self.physics.velocity;
        if self.physics.horizontal_collision
            && self.is_free(
                world,
                self.physics.bounding_box,
                velocity.up(0.6).down(y).up(y),
            )
        {
            self.physics.velocity.y = 0.3;
        }
    }

    fn is_free(&self, world: &dyn Blocks, bounding_box: Aabb, delta: Vec3) -> bool {
        let bounding_box = bounding_box.move_relative(delta);
        self.no_collision(world, &bounding_box) && !contains_any_liquid(world, bounding_box)
    }

    fn no_collision(&self, world: &dyn Blocks, aabb: &Aabb) -> bool {
        block_collisions(world, aabb).iter().all(VoxelShape::is_empty)
    }

    // ------------------------------------------------------------------
    // collision (azalea-physics/src/collision/mod.rs:46-377)
    // ------------------------------------------------------------------

    /// `Entity.move`: apply `movement` with collisions.
    fn move_colliding(&mut self, world: &dyn Blocks, mut movement: Vec3) {
        movement = self.maybe_back_off_from_edge(world, movement);
        let collide_result = self.collide(world, movement);

        let move_distance_sqr = collide_result.length_squared();

        if move_distance_sqr > EPSILON || movement.length_squared() - move_distance_sqr < EPSILON {
            *self.position += collide_result;
        }

        let x_collision = !math::equal(movement.x, collide_result.x);
        let z_collision = !math::equal(movement.z, collide_result.z);
        let horizontal_collision = x_collision || z_collision;
        self.physics.horizontal_collision = horizontal_collision;

        let vertical_collision = movement.y != collide_result.y;
        self.physics.vertical_collision = vertical_collision;
        let on_ground = vertical_collision && movement.y < 0.;
        self.physics.set_on_ground(on_ground);

        // Fall distance; fall damage itself is the server's business.
        if !self.physics.is_in_water() && collide_result.y < 0. {
            self.physics.fall_distance -= collide_result.y;
        }
        if self.physics.on_ground() {
            self.physics.fall_distance = 0.;
        }

        if horizontal_collision {
            let delta_movement = &self.physics.velocity;
            self.physics.velocity = Vec3 {
                x: if x_collision { 0. } else { delta_movement.x },
                y: delta_movement.y,
                z: if z_collision { 0. } else { delta_movement.z },
            };
        }
        if vertical_collision {
            self.physics.velocity.y = 0.;
        }
    }

    fn collide(&self, world: &dyn Blocks, movement: Vec3) -> Vec3 {
        let entity_bounding_box = self.physics.bounding_box;
        let collided_delta = if movement.length_squared() == 0.0 {
            movement
        } else {
            collide_bounding_box(world, movement, &entity_bounding_box)
        };

        let x_collision = movement.x != collided_delta.x;
        let y_collision = movement.y != collided_delta.y;
        let z_collision = movement.z != collided_delta.z;

        let on_ground = self.physics.on_ground() || y_collision && movement.y < 0.;

        let max_up_step = MAX_UP_STEP;
        if max_up_step > 0. && on_ground && (x_collision || z_collision) {
            let mut step_to_delta =
                collide_bounding_box(world, movement.with_y(max_up_step), &entity_bounding_box);
            let directly_up_delta = collide_bounding_box(
                world,
                Vec3::ZERO.with_y(max_up_step),
                &entity_bounding_box.expand_towards(Vec3::new(movement.x, 0., movement.z)),
            );
            if directly_up_delta.y < max_up_step {
                let target_movement = collide_bounding_box(
                    world,
                    movement.with_y(0.),
                    &entity_bounding_box.move_relative(directly_up_delta),
                ) + directly_up_delta;
                if target_movement.horizontal_distance_squared()
                    > step_to_delta.horizontal_distance_squared()
                {
                    step_to_delta = target_movement;
                }
            }

            if step_to_delta.horizontal_distance_squared()
                > collided_delta.horizontal_distance_squared()
            {
                return step_to_delta + collide_bounding_box(
                    world,
                    Vec3::ZERO.with_y(-step_to_delta.y + movement.y),
                    &entity_bounding_box.move_relative(step_to_delta),
                );
            }
        }

        collided_delta
    }

    fn maybe_back_off_from_edge(&self, world: &dyn Blocks, mut movement: Vec3) -> Vec3 {
        let is_staying_on_ground_surface = self.trying_to_crouch;
        let max_up_step = self.attributes.step_height.calculate() as f32;

        let is_backing_off = !self.abilities.flying
            && movement.y <= 0.
            && is_staying_on_ground_surface
            && self.is_above_ground(world, max_up_step);
        if !is_backing_off {
            return movement;
        }

        let min_movement = 0.05;
        let min_movement_x = movement.x.signum() * min_movement;
        let min_movement_z = movement.z.signum() * min_movement;

        while movement.x != 0.
            && self.can_fall_at_least(world, movement.x, 0., max_up_step as f64)
        {
            if movement.x.abs() <= min_movement {
                movement.x = 0.;
                break;
            }
            movement.x -= min_movement_x;
        }
        while movement.z != 0.
            && self.can_fall_at_least(world, 0., movement.z, max_up_step as f64)
        {
            if movement.z.abs() <= min_movement {
                movement.z = 0.;
                break;
            }
            movement.z -= min_movement_z;
        }
        while movement.x != 0.0
            && movement.z != 0.0
            && self.can_fall_at_least(world, movement.x, movement.z, max_up_step as f64)
        {
            if movement.x.abs() <= min_movement {
                movement.x = 0.;
            } else {
                movement.x -= min_movement_x;
            }
            if movement.z.abs() <= min_movement {
                movement.z = 0.;
            } else {
                movement.z -= min_movement_z;
            }
        }

        movement
    }

    fn is_above_ground(&self, world: &dyn Blocks, max_up_step: f32) -> bool {
        self.physics.on_ground()
            && self.physics.fall_distance < max_up_step as f64
            && !self.can_fall_at_least(
                world,
                0.,
                0.,
                max_up_step as f64 - self.physics.fall_distance,
            )
    }

    fn can_fall_at_least(
        &self,
        world: &dyn Blocks,
        delta_x: f64,
        delta_z: f64,
        max_up_step: f64,
    ) -> bool {
        let aabb = self.physics.bounding_box;
        let aabb = Aabb {
            min: Vec3 {
                x: aabb.min.x + EPSILON + delta_x,
                y: aabb.min.y - max_up_step - EPSILON,
                z: aabb.min.z + EPSILON + delta_z,
            },
            max: Vec3 {
                x: aabb.max.x - EPSILON + delta_x,
                y: aabb.min.y,
                z: aabb.max.z - EPSILON + delta_z,
            },
        };
        self.no_collision(world, &aabb)
    }
}

/// The blocks that collide with `aabb`, shifted to world coordinates.
///
/// This is `BlockCollisionsState::compute_next` without its cached sections
/// (`world_collisions.rs:74-125`): [`Blocks::block`] is already a hash lookup,
/// and a second cache would need the world's revision counters to stay correct.
fn block_collisions(world: &dyn Blocks, aabb: &Aabb) -> Vec<VoxelShape> {
    let origin = BlockPos {
        x: (aabb.min.x - EPSILON).floor() as i32 - 1,
        y: (aabb.min.y - EPSILON).floor() as i32 - 1,
        z: (aabb.min.z - EPSILON).floor() as i32 - 1,
    };
    let end = BlockPos {
        x: (aabb.max.x + EPSILON).floor() as i32 + 1,
        y: (aabb.max.y + EPSILON).floor() as i32 + 1,
        z: (aabb.max.z + EPSILON).floor() as i32 + 1,
    };
    let entity_shape = VoxelShape::from(aabb);

    let mut collisions = Vec::new();
    for item in Cursor3d::new(origin, end) {
        if item.iteration_type == CursorIterationType::Corner {
            continue;
        }
        let Some(block_state) = world.block(item.pos) else {
            continue;
        };
        if block_state.is_air() {
            continue;
        }
        if block_state.is_collision_shape_full() {
            let block = Aabb {
                min: item.pos.to_vec3_floored(),
                max: (item.pos + 1).to_vec3_floored(),
            };
            if !aabb.intersects_aabb(&block) {
                continue;
            }
            collisions.push(BLOCK_SHAPE.move_relative(item.pos.to_vec3_floored()));
            continue;
        }

        let shape = block_state
            .collision_shape(item.pos)
            .move_relative(item.pos.to_vec3_floored());
        if !Shapes::matches_anywhere(&shape, &entity_shape, |a, b| a && b) {
            continue;
        }
        collisions.push(shape);
    }
    collisions
}

fn collide_bounding_box(
    world: &dyn Blocks,
    movement: Vec3,
    entity_bounding_box: &Aabb,
) -> Vec3 {
    let collision_boxes = block_collisions(world, &entity_bounding_box.expand_towards(movement));
    collide_with_shapes(movement, *entity_bounding_box, &collision_boxes)
}

fn collide_with_shapes(
    mut movement: Vec3,
    mut entity_box: Aabb,
    collision_boxes: &[VoxelShape],
) -> Vec3 {
    if collision_boxes.is_empty() {
        return movement;
    }

    if movement.y != 0. {
        movement.y = Shapes::collide(Axis::Y, &entity_box, collision_boxes, movement.y);
        if movement.y != 0. {
            entity_box = entity_box.move_relative(Vec3::new(0., movement.y, 0.));
        }
    }

    let more_z_movement = movement.x.abs() < movement.z.abs();

    if more_z_movement && movement.z != 0. {
        movement.z = Shapes::collide(Axis::Z, &entity_box, collision_boxes, movement.z);
        if movement.z != 0. {
            entity_box = entity_box.move_relative(Vec3::new(0., 0., movement.z));
        }
    }

    if movement.x != 0. {
        movement.x = Shapes::collide(Axis::X, &entity_box, collision_boxes, movement.x);
        if movement.x != 0. {
            entity_box = entity_box.move_relative(Vec3::new(movement.x, 0., 0.));
        }
    }

    if !more_z_movement && movement.z != 0. {
        movement.z = Shapes::collide(Axis::Z, &entity_box, collision_boxes, movement.z);
    }

    movement
}

/// `travel::contains_any_liquid` (`travel.rs:326-345`).
fn contains_any_liquid(world: &dyn Blocks, bounding_box: Aabb) -> bool {
    let min = bounding_box.min.to_block_pos_floor();
    let max = bounding_box.max.to_block_pos_ceil();

    for x in min.x..max.x {
        for y in min.y..max.y {
            for z in min.z..max.z {
                let block_state = world.block(BlockPos::new(x, y, z)).unwrap_or_default();
                if !FluidState::from(block_state).is_empty() {
                    return true;
                }
            }
        }
    }

    false
}

/// `FluidState.of(block)` for one position; `None` when the block is unknown.
fn block_fluid(world: &dyn Blocks, pos: BlockPos) -> Option<FluidState> {
    let block_state = world.block(pos)?;
    Some(FluidState::from(block_state))
}

/// `fluids::get_fluid_flow` (`fluids.rs:183-242`).
fn get_fluid_flow(fluid: &FluidState, world: &dyn Blocks, pos: BlockPos) -> Vec3 {
    let mut z_flow: f64 = 0.;
    let mut x_flow: f64 = 0.;

    let cur_fluid_height = fluid.height();

    for direction in Direction::HORIZONTAL {
        let adjacent_block_pos = pos.offset_with_direction(direction);
        let adjacent_block_state = world.block(adjacent_block_pos).unwrap_or_default();
        let adjacent_fluid_state = FluidState::from(adjacent_block_state);

        if !fluid.affects_flow(&adjacent_fluid_state) {
            continue;
        }
        let mut adjacent_fluid_height = adjacent_fluid_state.height();
        let mut adjacent_height_difference: f32 = 0.;

        if adjacent_fluid_height == 0. {
            if !legacy_blocks_motion(adjacent_block_state) {
                let block_pos_below_adjacent = adjacent_block_pos.down(1);
                let fluid_below_adjacent = block_fluid(world, block_pos_below_adjacent)
                    .unwrap_or_default();
                if fluid.affects_flow(&fluid_below_adjacent) {
                    adjacent_fluid_height = fluid_below_adjacent.height();
                    if adjacent_fluid_height > 0. {
                        adjacent_height_difference =
                            cur_fluid_height - (adjacent_fluid_height - 0.8888889);
                    }
                }
            }
        } else if adjacent_fluid_height > 0. {
            adjacent_height_difference = cur_fluid_height - adjacent_fluid_height;
        }

        if adjacent_height_difference != 0. {
            x_flow += (direction.x() as f32 * adjacent_height_difference) as f64;
            z_flow += (direction.z() as f32 * adjacent_height_difference) as f64;
        }
    }

    let mut flow = Vec3::new(x_flow, 0., z_flow);
    if fluid.falling {
        for direction in Direction::HORIZONTAL {
            let adjacent_block_pos = pos.offset_with_direction(direction);
            if is_solid_face(fluid, world, adjacent_block_pos, direction)
                || is_solid_face(fluid, world, adjacent_block_pos.up(1), direction)
            {
                flow = flow.normalize() + Vec3::new(0., -6., 0.);
                break;
            }
        }
    }

    flow.normalize()
}

fn is_solid_face(
    fluid: &FluidState,
    world: &dyn Blocks,
    adjacent_pos: BlockPos,
    direction: Direction,
) -> bool {
    let block_state = world.block(adjacent_pos).unwrap_or_default();
    let fluid_state = block_fluid(world, adjacent_pos).unwrap_or_default();
    if fluid_state.is_same_kind(fluid) {
        return false;
    }
    if direction == Direction::Up {
        return true;
    }
    let registry_block: azalea_registry::builtin::BlockKind = block_state.into();
    if matches!(
        registry_block,
        azalea_registry::builtin::BlockKind::Ice | azalea_registry::builtin::BlockKind::FrostedIce
    ) {
        return false;
    }
    // BlockBehavior.isFaceSturdy is a TODO in azalea too (`fluids.rs:270-284`).
    false
}

/// `BlockBehavior.jump_factor` of the block at the player's feet or below it
/// (`lib.rs:502-524`).
fn block_jump_factor(world: &dyn Blocks, position: Position) -> f32 {
    let block_at_pos = world.block(BlockPos::from(position));
    if let Some(block) = block_at_pos {
        let factor = Box::<dyn BlockTrait>::from(block).behavior().jump_factor;
        if factor != 1. {
            return factor;
        }
    }

    if let Some(block) = world.block(get_block_pos_below_that_affects_movement(position)) {
        Box::<dyn BlockTrait>::from(block).behavior().jump_factor
    } else {
        1.
    }
}

/// `tick_controls` (`azalea-client/src/plugins/movement.rs:265-286`).
fn tick_controls(held: &Held) -> Vec2 {
    let mut forward_impulse: f32 = 0.;
    let mut left_impulse: f32 = 0.;

    if held.forward {
        forward_impulse += 1.;
    } else if held.back {
        forward_impulse -= 1.;
    }
    if held.left {
        left_impulse += 1.;
    } else if held.right {
        left_impulse -= 1.;
    }

    Vec2::new(left_impulse, forward_impulse).normalized()
}

/// `LocalPlayer.modifyInput` (`movement.rs:429-468`).
fn modify_input(mut move_vector: Vec2, moving_slowly: bool, attributes: &Attributes) -> Vec2 {
    if move_vector.length_squared() == 0. {
        return move_vector;
    }

    move_vector *= 0.98;
    if moving_slowly {
        let sneaking_speed = attributes.sneaking_speed.calculate() as f32;
        move_vector *= sneaking_speed;
    }

    modify_input_speed_for_square_movement(move_vector)
}

fn modify_input_speed_for_square_movement(move_vector: Vec2) -> Vec2 {
    let length = move_vector.length();
    if length == 0. {
        return move_vector;
    }
    let scaled_to_inverse_length = move_vector * (1. / length);
    let dist = distance_to_unit_square(scaled_to_inverse_length);
    let scale = (length * dist).min(1.);
    scaled_to_inverse_length * scale
}

fn distance_to_unit_square(v: Vec2) -> f32 {
    let x = v.x.abs();
    let y = v.y.abs();
    let ratio = if y > x { x / y } else { y / x };
    (1. + ratio * ratio).sqrt()
}

/// `get_friction_influenced_speed` (`lib.rs:486-500`).
fn get_friction_influenced_speed(
    physics: &Physics,
    attributes: &Attributes,
    friction: f32,
    sprinting: bool,
) -> f32 {
    if physics.on_ground() {
        let speed = attributes.movement_speed.calculate() as f32;
        speed * (0.21600002f32 / (friction * friction * friction))
    } else if sprinting {
        0.025999999
    } else {
        0.02
    }
}

/// `travel::get_fluid_falling_adjusted_movement` (`travel.rs:228-252`).
fn get_fluid_falling_adjusted_movement(
    gravity: f64,
    moving_down: bool,
    new_velocity: Vec3,
    sprinting: bool,
) -> Vec3 {
    if gravity != 0. && !sprinting {
        let new_y_velocity = if moving_down
            && (new_velocity.y - 0.005).abs() >= 0.003
            && f64::abs(new_velocity.y - gravity / 16.0) < 0.003
        {
            -0.003
        } else {
            new_velocity.y - gravity / 16.0
        };

        Vec3 {
            x: new_velocity.x,
            y: new_y_velocity,
            z: new_velocity.z,
        }
    } else {
        new_velocity
    }
}

/// `has_correct_tool_for_drops` (`azalea-entity/src/mining.rs:48-65`).
fn has_correct_tool_for_drops(block: &dyn BlockTrait, item: &ItemStack) -> bool {
    if !block.behavior().requires_correct_tool_for_drops {
        return true;
    }
    let Some(tool) = item.get_component::<Tool>() else {
        return false;
    };
    let registry_block = block.as_block_kind();
    for rule in &tool.rules {
        if let Some(correct) = rule.correct_for_drops
            && rule.blocks.contains(registry_block)
        {
            return correct;
        }
    }
    false
}

/// `base_destroy_speed` (`azalea-entity/src/mining.rs:118-129`).
fn base_destroy_speed(block: azalea_registry::builtin::BlockKind, item: &ItemStack) -> f32 {
    let tool = item.get_component::<Tool>();
    let Some(tool) = tool else { return 1. };
    for rule in &tool.rules {
        if let Some(speed) = rule.speed
            && rule.blocks.contains(block)
        {
            return speed;
        }
    }
    tool.default_mining_speed
}

#[cfg(test)]
mod tests;
