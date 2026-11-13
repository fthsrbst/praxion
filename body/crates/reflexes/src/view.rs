//! The perception view of one tick: the only input a reflex evaluates.
//!
//! [`View::capture`] asks the perception API for everything the first-night
//! reflex set needs, once per tick, and then the reflex functions are pure
//! over the resulting value. Nothing here reads the world directly: the world
//! enters only through `praxion_perception`.
//!
//! The view is fixed-size where the hot path allows it: the nearest creeper,
//! up to eight melee mobs, up to four Endermen and up to eight arrows. The inventory is the
//! one exception, because the perception API returns a `Vec` and cloning it is
//! its price; only the best food and the best weapon survive into the view.
//!
//! # Unknown is a value, not air
//!
//! A cell perception cannot answer is [`Cell::Unknown`], never "air" (§5.1).
//! An unknown cell never makes a trigger fire (only a *known* lava or fire
//! does) and never counts as safe ground or a passable body cell, so a reflex
//! can neither step into one nor treat one as a floor. [`View::feet_state`],
//! [`View::head_state`] and [`View::below_state`] are the last known states for
//! display and logging: when the cell is unknown they hold `AIR`, which is why
//! no decision reads them for safety. The decision path asks [`Cell`] again
//! through the [`KnownBlocks`] seam ([`View::is_safe_standing`], `Motion`), and
//! the planner stops a path at the first unknown cell (`planner::world`).

use azalea_inventory::{components, ItemStack};
use azalea_registry::builtin::{BlockKind, EntityKind, ItemKind};
use praxion_perception::{Answer, BlockFact, Caller, Perception};
use praxion_planner::{Block, KnownBlocks, blocks};
use praxion_world::{BlockPos, BlockState, Vec3, World};

use crate::reflex::ReflexState;

/// Endermen tracked for the look guard. Four is more than a first night sees
/// in one view.
pub const MAX_ENDERMEN: usize = 4;

/// Arrows tracked for the stray-arrow reflex.
pub const MAX_ARROWS: usize = 8;

/// Melee mobs tracked beyond the nearest one ([`View::melee_others`]). The
/// nearest mob alone is not enough: when the running skill is engaged with it,
/// a second mob of another kind must still reach the melee trigger (decision
/// 2026-11-10 "The skill that started a fight owns it").
pub const MAX_MELEE_OTHERS: usize = 7;

/// Entities beyond this are not threats yet; the sight range itself is 64.
const THREAT_RADIUS: u32 = 64;

/// An arrow farther than this cannot reach the player before it is harmless.
const ARROW_RADIUS: u32 = 32;

/// How deep a drop the fall guard looks for, in blocks.
const DROP_PROBE: i32 = 5;

/// One visible mob, reduced to what a reflex uses.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EntityView {
    pub id: i32,
    pub kind: EntityKind,
    pub position: Vec3,
    pub distance: f64,
}

/// A block cell as a reflex decision knows it: the state a sighting answered,
/// or [`Cell::Unknown`] when nothing ever saw it.
///
/// Every safety question goes through this type, so "unknown" can never be
/// answered as air. `Unknown` is false for every predicate: it is not lava, not
/// fire, not solid and not passable. The predicates are deliberately one-sided:
/// only a *known* hazard triggers a reflex, and only a *known* safe cell is one
/// a reflex may step into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cell {
    Known(BlockState),
    Unknown,
}

impl Cell {
    /// The state, when a sighting answered.
    pub fn state(self) -> Option<BlockState> {
        match self {
            Cell::Known(state) => Some(state),
            Cell::Unknown => None,
        }
    }

    /// Whether a sighting answered this cell.
    pub fn is_known(self) -> bool {
        matches!(self, Cell::Known(_))
    }

    /// Known lava. An unknown cell is never lava: a lava trigger fires only on
    /// a fact.
    pub fn is_lava(self) -> bool {
        matches!(self, Cell::Known(state) if BlockKind::from(state) == BlockKind::Lava)
    }

    /// Known water.
    pub fn is_water(self) -> bool {
        matches!(self, Cell::Known(state) if BlockKind::from(state) == BlockKind::Water)
    }

    /// Known fire or soul fire.
    pub fn is_fire(self) -> bool {
        matches!(self, Cell::Known(state) if is_fire(state))
    }

    /// Known to be a full floor. Unknown is not a floor.
    pub fn is_solid(self) -> bool {
        matches!(self, Cell::Known(state) if blocks::is_solid(state))
    }

    /// Known to be a cell the body fits into. Unknown is not passable.
    pub fn is_passable(self) -> bool {
        matches!(self, Cell::Known(state) if blocks::is_passable(state))
    }

    /// Whether a reflex may put its feet here: a known cell the body fits
    /// into. Lava, fire and every other hazard is not passable and an unknown
    /// cell is not either, so this one test refuses both.
    pub fn is_safe_to_enter(self) -> bool {
        self.is_passable()
    }

    /// Ask a [`KnownBlocks`] source. `Block::Unknown` stays unknown.
    pub fn probe(blocks: &mut dyn KnownBlocks, pos: BlockPos) -> Self {
        match blocks.block(pos) {
            Block::Known { state, .. } => Cell::Known(state),
            Block::Unknown => Cell::Unknown,
        }
    }
}

/// An arrow and what the reflex needs to dodge it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ArrowView {
    pub id: i32,
    pub position: Vec3,
    pub distance: f64,
    /// The arrow moved toward the player since the previous sighting.
    pub toward: bool,
    /// Which way to side-step, from the arrow's flight line: `true` is the
    /// body's right hand, `false` its left.
    pub strafe_right: bool,
}

/// Where the previous tick saw each arrow, for a heading.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ArrowMemory {
    entries: [Option<(i32, Vec3)>; MAX_ARROWS],
}

impl ArrowMemory {
    fn previous(&self, id: i32) -> Option<Vec3> {
        self.entries
            .iter()
            .flatten()
            .find(|(seen, _)| *seen == id)
            .map(|(_, pos)| *pos)
    }

    /// Replace the memory with this tick's arrows.
    pub fn update(&mut self, arrows: &[Option<ArrowView>; MAX_ARROWS]) {
        self.entries = [None; MAX_ARROWS];
        for (slot, arrow) in self.entries.iter_mut().zip(arrows.iter()) {
            if let Some(arrow) = arrow {
                *slot = Some((arrow.id, arrow.position));
            }
        }
    }
}

/// The best food in the inventory, by nutrition then saturation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FoodChoice {
    /// Index in `World::inventory`, not the hotbar number.
    pub slot: u16,
    pub kind: ItemKind,
    pub nutrition: i32,
    pub saturation: f32,
}

/// The best weapon in the inventory, by the small material table below.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WeaponChoice {
    pub slot: u16,
    pub kind: ItemKind,
    pub rank: u8,
}

impl WeaponChoice {
    /// Whether the weapon is a sword (item tag `#swords`). "Armed" means a
    /// sword, the same test as `night_watch` and the brain (decision 2026-11-10
    /// "Unarmed at night: avoid contact"); an axe is still the best thing to
    /// hit with, but it does not make the body armed.
    pub fn is_sword(&self) -> bool {
        let name = praxion_gamedata::normalize(self.kind.to_str());
        praxion_gamedata::item_tag("swords").is_some_and(|swords| swords.contains(&name))
    }
}

/// Everything the reflex set sees this tick.
#[derive(Clone, Debug)]
pub struct View {
    pub tick: u64,
    /// The player's feet.
    pub position: Vec3,
    pub eye: Vec3,
    pub rotation: (f32, f32),
    pub health: Option<f32>,
    pub food: Option<u32>,
    pub day_time: u64,
    pub feet: BlockPos,
    pub eye_cell: BlockPos,
    /// The last known state at the feet. `AIR` here means "air" or "nothing
    /// was seen": use [`View::is_safe_standing`] or [`Cell`] for anything that
    /// decides whether the body may be there.
    pub feet_state: BlockState,
    /// The last known state in the head cell; see [`View::feet_state`].
    pub head_state: BlockState,
    /// The last known state below the feet; see [`View::feet_state`].
    pub below_state: BlockState,
    /// The head cell is known water.
    pub head_in_water: bool,
    /// The feet cell is known water.
    pub feet_in_water: bool,
    /// The feet or the cell below the feet is known lava. A cell nobody saw is
    /// not lava: this only ever says a fact.
    pub feet_in_lava: bool,
    /// A known fire or soul-fire block at the feet or the head.
    pub on_fire: bool,
    /// The nearest lava within two blocks.
    pub lava: Option<BlockFact>,
    /// The nearest water within eight blocks.
    pub water: Option<BlockFact>,
    /// The cell one step ahead along the current yaw.
    pub next_cell: BlockPos,
    /// Known lava in or just below [`View::next_cell`]; never an unknown cell.
    pub next_cell_lava: bool,
    /// How far the drop below [`View::next_cell`] is, 0 for level ground and
    /// the deepest probe (5) when no floor was seen.
    pub next_drop: u8,
    pub creeper: Option<EntityView>,
    /// The nearest zombie, skeleton or spider.
    pub melee: Option<EntityView>,
    /// The next nearest zombies, skeletons and spiders after [`View::melee`],
    /// nearest first; see [`View::melee_mobs`].
    pub melee_others: [Option<EntityView>; MAX_MELEE_OTHERS],
    pub endermen: [Option<EntityView>; MAX_ENDERMEN],
    pub enderman_count: usize,
    pub arrows: [Option<ArrowView>; MAX_ARROWS],
    pub arrow_count: usize,
    pub best_food: Option<FoodChoice>,
    pub best_weapon: Option<WeaponChoice>,
}

impl View {
    /// Ask perception for this tick's facts. The caller must have called
    /// [`Perception::begin_tick`] first.
    ///
    /// This is the one place the view and the reflex memory meet: the own
    /// entity facts (air, burning) are not part of the fixed-size view, they
    /// are remembered in [`ReflexState`] where the drowning and fire triggers
    /// read them.
    pub fn capture(perception: &mut Perception, world: &World, state: &mut ReflexState) -> Self {
        let previous_arrows = state.arrows;
        let position = perception.own_position(world).value().copied().unwrap_or(Vec3::ZERO);
        let eye = perception.eye(world).unwrap_or(position);
        let rotation = perception.own_rotation(world).value().copied().unwrap_or((0.0, 0.0));
        let health = perception.own_health(world).value().map(|health| health.health());
        let food = perception.own_food(world).value().copied();
        let day_time = perception.day_time(world).value().copied().unwrap_or(0);
        let feet = position.to_block_pos_floor();
        let eye_cell = eye.to_block_pos_floor();

        // The own entity facts, as F3 facts with no ray. `Unknown` here means
        // the server never said, which is not the same as "full air" or "not
        // burning"; the reflex keeps its simulation for the air case only.
        state.set_own_facts(
            perception.air(world).value().copied(),
            perception.burning(world).value().copied(),
        );
        // Hearing, the same way: a server fact of this tick, classified once
        // and kept in the state beside the own facts (see `crate::hearing`).
        // It costs no ray and no ledger entry.
        state.set_heard(crate::hearing::heard_sounds(perception, world));

        let feet_cell = cell(perception, world, feet);
        // The head cell is one above the feet unless the eye already is there.
        let head_cell = if eye_cell == feet { feet.up(1) } else { eye_cell };
        let head_state = cell(perception, world, head_cell);
        let below_state = cell(perception, world, feet.down(1));

        let feet_in_water = feet_cell.is_water();
        // Drowning is about the head, not the feet: a player standing in a
        // shallow pond with the head in air is not drowning.
        let head_in_water = head_state.is_water();
        let feet_in_lava = feet_cell.is_lava() || below_state.is_lava();

        let on_fire = feet_cell.is_fire()
            || head_state.is_fire()
            || cell(perception, world, feet.up(1)).is_fire();

        let lava = perception
            .nearest_block(Caller::Reflex, world, BlockKind::Lava, 2)
            .value()
            .cloned();
        let water = perception
            .nearest_block(Caller::Reflex, world, BlockKind::Water, 8)
            .value()
            .cloned();

        let next_cell = next_cell(feet, rotation.0);
        let next_cell_lava = cell(perception, world, next_cell).is_lava()
            || cell(perception, world, next_cell.down(1)).is_lava();
        let next_drop = drop_below(perception, world, next_cell);

        // The three raw cells are display values: see the module note. An
        // unknown cell shows as air, and no decision reads them for safety.
        let feet_state = feet_cell.state().unwrap_or(BlockState::AIR);
        let head_state = head_state.state().unwrap_or(BlockState::AIR);
        let below_state = below_state.state().unwrap_or(BlockState::AIR);

        let mut view = Self {
            tick: perception.tick(),
            position,
            eye,
            rotation,
            health,
            food,
            day_time,
            feet,
            eye_cell,
            feet_state,
            head_state,
            below_state,
            head_in_water,
            feet_in_water,
            feet_in_lava,
            on_fire,
            lava,
            water,
            next_cell,
            next_cell_lava,
            next_drop,
            creeper: None,
            melee: None,
            melee_others: [None; MAX_MELEE_OTHERS],
            endermen: [None; MAX_ENDERMEN],
            enderman_count: 0,
            arrows: [None; MAX_ARROWS],
            arrow_count: 0,
            best_food: None,
            best_weapon: None,
        };
        view.classify_entities(perception, world, &previous_arrows);
        view.classify_inventory(perception, world);
        view
    }

    fn classify_entities(
        &mut self,
        perception: &mut Perception,
        world: &World,
        previous_arrows: &ArrowMemory,
    ) {
        let Answer::Known { value: entities, .. } =
            perception.entities_near(Caller::Reflex, world, THREAT_RADIUS, praxion_perception::EntityFilter::Any)
        else {
            return;
        };
        for entity in entities {
            let view = EntityView {
                id: entity.id,
                kind: entity.kind,
                position: entity.position,
                distance: entity.distance,
            };
            match entity.kind {
                EntityKind::Creeper => keep_nearest(&mut self.creeper, view),
                EntityKind::Zombie | EntityKind::Skeleton | EntityKind::Spider => {
                    self.keep_melee(view)
                }
                EntityKind::Enderman => {
                    if self.enderman_count < MAX_ENDERMEN {
                        self.endermen[self.enderman_count] = Some(view);
                        self.enderman_count += 1;
                    }
                }
                EntityKind::Arrow | EntityKind::SpectralArrow => {
                    if entity.distance > f64::from(ARROW_RADIUS) || self.arrow_count >= MAX_ARROWS {
                        continue;
                    }
                    let previous = previous_arrows.previous(entity.id);
                    let motion = previous.map_or(Vec3::ZERO, |previous| entity.position - previous);
                    let toward = previous.is_some_and(|_| {
                        let to_player = self.position + Vec3::new(0.0, 0.9, 0.0) - entity.position;
                        motion.dot(to_player) > 0.0
                            && (motion.x * motion.x + motion.z * motion.z) > 0.0025
                    });
                    self.arrows[self.arrow_count] = Some(ArrowView {
                        id: entity.id,
                        position: entity.position,
                        distance: entity.distance,
                        toward,
                        strafe_right: strafe_right(self.rotation.0, self.eye, entity.position, motion),
                    });
                    self.arrow_count += 1;
                }
                _ => {}
            }
        }
    }

    fn classify_inventory(&mut self, perception: &mut Perception, world: &World) {
        let Answer::Known { value: inventory, .. } = perception.inventory(world) else {
            return;
        };
        for (index, stack) in inventory.iter().enumerate() {
            let ItemStack::Present(data) = stack else {
                continue;
            };
            if let Some(food) = data.get_component::<components::Food>() {
                let choice = FoodChoice {
                    slot: index as u16,
                    kind: data.kind,
                    nutrition: food.nutrition,
                    saturation: food.saturation,
                };
                if self.best_food.is_none_or(|best| {
                    (choice.nutrition, choice.saturation) > (best.nutrition, best.saturation)
                }) {
                    self.best_food = Some(choice);
                }
            }
            if let Some(rank) = weapon_rank(data.kind) {
                let choice = WeaponChoice {
                    slot: index as u16,
                    kind: data.kind,
                    rank,
                };
                if self.best_weapon.is_none_or(|best| choice.rank > best.rank) {
                    self.best_weapon = Some(choice);
                }
            }
        }
    }

    /// Every tracked zombie, skeleton and spider, nearest first.
    pub fn melee_mobs(&self) -> impl Iterator<Item = EntityView> + '_ {
        self.melee
            .into_iter()
            .chain(self.melee_others.iter().flatten().copied())
    }

    /// The tracked melee mob with entity id `id`.
    pub fn melee_mob(&self, id: i32) -> Option<EntityView> {
        self.melee_mobs().find(|mob| mob.id == id)
    }

    /// Insert a melee mob into [`View::melee`] and [`View::melee_others`],
    /// which together stay sorted nearest first; the farthest falls off when
    /// all slots are full.
    fn keep_melee(&mut self, candidate: EntityView) {
        let mut carry = Some(candidate);
        let slots = std::iter::once(&mut self.melee).chain(self.melee_others.iter_mut());
        for slot in slots {
            let Some(mob) = carry else {
                break;
            };
            if slot.is_none_or(|current| mob.distance < current.distance) {
                carry = slot.replace(mob);
            }
        }
    }

    /// Whether a mob is within `range` blocks of the player, horizontally.
    /// Mobs stand on the ground, so the eye-to-feet distance would shrink a
    /// "three blocks away" creeper to 3.4 and lose the lethal class.
    pub fn threat_within(&self, mob: Option<EntityView>, range: u32) -> bool {
        mob.is_some_and(|mob| self.horizontal_to(mob) <= f64::from(range))
    }

    /// The horizontal (x/z) distance to a mob.
    pub fn horizontal_to(&self, mob: EntityView) -> f64 {
        self.horizontal_to_point(mob.position)
    }

    /// The horizontal (x/z) distance from the feet to a point in the world,
    /// e.g. where a sound was heard.
    pub fn horizontal_to_point(&self, position: Vec3) -> f64 {
        let dx = self.position.x - position.x;
        let dz = self.position.z - position.z;
        (dx * dx + dz * dz).sqrt()
    }

    /// The horizontal (x/z) distance from the feet to a block's centre.
    pub fn horizontal_to_block(&self, pos: BlockPos) -> f64 {
        let dx = self.position.x - (f64::from(pos.x) + 0.5);
        let dz = self.position.z - (f64::from(pos.z) + 0.5);
        (dx * dx + dz * dz).sqrt()
    }

    /// The nearest Enderman head within 64 blocks, if any. "Nearest" is by
    /// horizontal distance to the body, like [`View::horizontal_to`]: the
    /// Endermen are stored in sighting order, which says nothing about which
    /// one matters.
    pub fn enderman_head(&self) -> Option<Vec3> {
        self.endermen
            .iter()
            .flatten()
            .min_by(|a, b| self.horizontal_to(**a).total_cmp(&self.horizontal_to(**b)))
            .map(|e| e.position + Vec3::new(0.0, 1.5, 0.0))
    }

    /// The nearest arrow heading at the player.
    pub fn incoming_arrow(&self) -> Option<ArrowView> {
        self.arrows.iter().flatten().copied().filter(|a| a.toward).min_by(|a, b| {
            a.distance.total_cmp(&b.distance)
        })
    }

    /// Ground at the feet, passable body cells, no lava and no fire: a cell a
    /// step back may return to.
    ///
    /// Every cell is asked again through the [`KnownBlocks`] seam instead of
    /// trusting the view's display states, because an unknown cell is not air:
    /// a floor nobody saw is not a floor and a body cell nobody saw is not
    /// passable.
    pub fn is_safe_standing(&self, blocks: &mut dyn KnownBlocks) -> bool {
        let feet = Cell::probe(blocks, self.feet);
        let head = Cell::probe(blocks, self.eye_cell);
        let below = Cell::probe(blocks, self.feet.down(1));
        // Lava and fire are hazards, so they are not passable and not solid:
        // the three predicates below already refuse them, and an unknown cell
        // fails every one of them.
        !self.on_fire
            && feet.is_known()
            && feet.is_safe_to_enter()
            && head.is_known()
            && head.is_passable()
            && below.is_known()
            && below.is_solid()
    }
}

fn keep_nearest(slot: &mut Option<EntityView>, candidate: EntityView) {
    if slot.is_none_or(|current| candidate.distance < current.distance) {
        *slot = Some(candidate);
    }
}

/// A cell as perception answers it, with `Unknown` kept as a value.
fn cell(perception: &mut Perception, world: &World, pos: BlockPos) -> Cell {
    match perception.block(Caller::Reflex, world, pos).value() {
        Some(fact) => Cell::Known(fact.state),
        None => Cell::Unknown,
    }
}

/// Which way the body side-steps for an arrow: away from the arrow's flight
/// line, measured in the body's own left/right.
///
/// An arrow that will pass on the right is dodged to the left and the other way
/// round, so the step always increases the distance between the body and the
/// line the arrow is travelling along. When the body sits exactly on that line
/// (a shot straight at the eye) there is no offset to read, and the side is
/// taken from the arrow's own lateral drift so the body never steps into it.
///
/// The body's right hand is `-x` at yaw 0 (facing `+z`), which is the same
/// convention `praxion_headless`'s movement uses for `Held::right`.
fn strafe_right(yaw: f32, eye: Vec3, arrow: Vec3, motion: Vec3) -> bool {
    let radians = f64::from(yaw).to_radians();
    let right = Vec3::new(-radians.cos(), 0.0, -radians.sin());
    let to_player = Vec3::new(eye.x - arrow.x, 0.0, eye.z - arrow.z);
    let flight = Vec3::new(motion.x, 0.0, motion.z);
    let length = (flight.x * flight.x + flight.z * flight.z).sqrt();
    let offset = if length < 1.0e-6 {
        to_player
    } else {
        let unit = flight * (1.0 / length);
        let along = to_player.x * unit.x + to_player.z * unit.z;
        Vec3::new(to_player.x - unit.x * along, 0.0, to_player.z - unit.z * along)
    };
    let lateral = offset.x * right.x + offset.z * right.z;
    if lateral.abs() > 1.0e-6 {
        return lateral > 0.0;
    }
    let drift = flight.x * right.x + flight.z * right.z;
    -drift > 0.0
}

fn is_fire(state: BlockState) -> bool {
    matches!(BlockKind::from(state), BlockKind::Fire | BlockKind::SoulFire)
}

/// The cardinal cell one step ahead in the current yaw. Yaw 0 faces +z and
/// yaw -90 faces +x, exactly like [`praxion_planner::aim`].
fn next_cell(feet: BlockPos, yaw: f32) -> BlockPos {
    let radians = f64::from(yaw).to_radians();
    let dx = -radians.sin();
    let dz = radians.cos();
    if dx.abs() > dz.abs() {
        feet.east(dx.signum() as i32)
    } else {
        feet.south(dz.signum() as i32)
    }
}

/// The drop below `from`: 0 when the floor is at the same level, the depth
/// otherwise, and [`DROP_PROBE`] when no floor is seen within the probe. A cell
/// nobody saw is not a floor, so an unknown column reads as the deepest drop
/// and the fall guard refuses it.
fn drop_below(perception: &mut Perception, world: &World, from: BlockPos) -> u8 {
    for depth in 1..=DROP_PROBE {
        let probe = cell(perception, world, from.down(depth));
        if probe.is_water() {
            return 0;
        }
        if probe.is_solid() {
            return (depth - 1) as u8;
        }
    }
    DROP_PROBE as u8
}

/// The weapon ladder used to pick "the best weapon": the shared table in
/// `praxion-gamedata`. Fists are rank 0 and implicit when nothing is found.
fn weapon_rank(kind: ItemKind) -> Option<u8> {
    praxion_gamedata::weapon_rank(kind.to_str())
}
