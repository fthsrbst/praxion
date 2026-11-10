//! The first-night reflex set: one pure function per reflex.
//!
//! Each [`ReflexKind`] has a trigger over [`View`] facts and an action. The
//! evaluation order is fixed by [`ReflexKind::priority`]; a trigger that is
//! visible but held back by the human floor is still logged as visible, so the
//! latency gate is the only reason it waited.
//!
//! The first-night set (session 7): creeper, melee (zombie, skeleton, spider),
//! arrows, lava, drowning, fire, starvation, plus two guards that are not
//! actions of their own: the Enderman look guard ([`crate::EndermanGuard`]) and
//! the fall guard inside the motion routines ([`crate::Motion`]).

use azalea_registry::builtin::{EntityKind, ItemKind};
use praxion_planner::KnownBlocks;
use praxion_world::{BlockPos, Vec3};

use crate::settings::{HUMAN_FLOOR_TICKS, Settings};
use crate::view::{ArrowMemory, EntityView, View};

/// Vanilla air supply, in ticks.
pub const FULL_AIR_TICKS: u32 = 300;

/// A creeper inside this many blocks is an immediate threat and may act on the
/// first tick (`lethal_fast`).
pub const CREEPER_LETHAL_RANGE: f64 = 3.0;

/// A running melee reflex holds the body until its mob is this much farther
/// than `melee_range` (horizontal, blocks). Without the margin a mob that
/// strafes on the trigger line ends and restarts the reflex every few ticks
/// (run 9, decision 2026-11-10 "The skill that started a fight owns it").
pub const MELEE_EXIT_MARGIN: f64 = 2.0;

/// After any melee reflex ends, it may not start again for this many ticks, so
/// the skill underneath gets the body back for at least a second.
pub const MELEE_COOLDOWN_TICKS: u64 = 20;

/// The two latency classes of architecture §3.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LatencyClass {
    /// May act only after its trigger has been visible for the human floor
    /// (three ticks, 150 ms).
    HumanFloor,
    /// May act on the first tick the trigger is visible.
    LethalFast,
}

/// The seven first-night reflexes, highest priority first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ReflexKind {
    Lava,
    Drowning,
    Fire,
    Creeper,
    Arrow,
    Melee,
    Starvation,
}

impl ReflexKind {
    pub const ALL: [ReflexKind; 7] = [
        ReflexKind::Lava,
        ReflexKind::Drowning,
        ReflexKind::Fire,
        ReflexKind::Creeper,
        ReflexKind::Arrow,
        ReflexKind::Melee,
        ReflexKind::Starvation,
    ];

    pub const fn index(self) -> usize {
        match self {
            ReflexKind::Lava => 0,
            ReflexKind::Drowning => 1,
            ReflexKind::Fire => 2,
            ReflexKind::Creeper => 3,
            ReflexKind::Arrow => 4,
            ReflexKind::Melee => 5,
            ReflexKind::Starvation => 6,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            ReflexKind::Lava => "lava",
            ReflexKind::Drowning => "drowning",
            ReflexKind::Fire => "fire",
            ReflexKind::Creeper => "creeper",
            ReflexKind::Arrow => "arrow",
            ReflexKind::Melee => "melee",
            ReflexKind::Starvation => "starvation",
        }
    }

    /// Higher wins. Death in one tick outranks death in ten; damage control
    /// outranks a fight.
    pub const fn priority(self) -> u8 {
        match self {
            ReflexKind::Lava => 70,
            ReflexKind::Drowning => 60,
            ReflexKind::Fire => 50,
            ReflexKind::Creeper => 40,
            ReflexKind::Arrow => 35,
            ReflexKind::Melee => 30,
            ReflexKind::Starvation => 20,
        }
    }

    /// The class of this trigger now. Only the creeper changes class: inside
    /// three blocks it may act on the first tick.
    pub fn latency_class(self, view: &View) -> LatencyClass {
        match self {
            ReflexKind::Lava | ReflexKind::Drowning | ReflexKind::Fire | ReflexKind::Arrow => {
                LatencyClass::LethalFast
            }
            ReflexKind::Creeper => match view.creeper {
                Some(creeper) if view.horizontal_to(creeper) <= CREEPER_LETHAL_RANGE => {
                    LatencyClass::LethalFast
                }
                _ => LatencyClass::HumanFloor,
            },
            ReflexKind::Melee | ReflexKind::Starvation => LatencyClass::HumanFloor,
        }
    }

    /// Whether the trigger is present this tick. Pure.
    pub fn triggered(self, view: &View, state: &ReflexState, settings: &Settings) -> bool {
        match self {
            ReflexKind::Lava => {
                view.feet_in_lava
                    || view.next_cell_lava
                    || view
                        .lava
                        .as_ref()
                        .is_some_and(|lava| view.horizontal_to_block(lava.pos) <= 1.5)
            }
            // The air fact from the server when it exists, the simulation
            // otherwise (see [`ReflexState::air`]). A block the view could not
            // answer never makes this fire; a low air supply does, because the
            // server only lowers it while the head is under water.
            ReflexKind::Drowning => {
                view.head_in_water && state.air() <= settings.air_surface_threshold
            }
            // The server's shared flags when they exist, the fire block under
            // the body otherwise: both mean "the body is burning now".
            ReflexKind::Fire => state.burning() || view.on_fire,
            ReflexKind::Creeper => view.threat_within(view.creeper, settings.creeper_flee_range),
            ReflexKind::Arrow => view.incoming_arrow().is_some(),
            ReflexKind::Melee => melee_target(view, state, settings)
                .is_some_and(|mob| view.horizontal_to(mob) <= f64::from(settings.melee_range)),
            ReflexKind::Starvation => view
                .food
                .is_some_and(|food| food <= settings.eat_hard_threshold),
        }
    }

    /// Whether a *running* reflex of this kind still has its trigger. The same
    /// as [`ReflexKind::triggered`] except for melee, which keeps the body
    /// until its mob is [`MELEE_EXIT_MARGIN`] beyond the trigger range: the
    /// start and the end of a fight are two different lines, so a mob on the
    /// trigger line cannot flip the owner every few ticks.
    pub fn holds(self, view: &View, state: &ReflexState, settings: &Settings) -> bool {
        match self {
            ReflexKind::Melee => melee_target(view, state, settings).is_some_and(|mob| {
                view.horizontal_to(mob) <= f64::from(settings.melee_range) + MELEE_EXIT_MARGIN
            }),
            _ => self.triggered(view, state, settings),
        }
    }
}

/// What a reflex asks the body to do. The motion routine turns it into a
/// `ControlFrame`; a reflex never calls a skill.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ReflexAction {
    /// Get `distance` blocks away from `from` along a planner path.
    Flee { from: Vec3, distance: f32 },
    /// Fight `target` with the best weapon (or fists). `weapon` is the
    /// inventory slot the driver should select; `None` means fists.
    Attack { target: i32, weapon: Option<u16> },
    /// Side-step for a few ticks.
    Strafe { right: bool },
    /// Return to the last cell that was safe to stand in.
    StepBack { to: BlockPos },
    /// Ascend in water.
    SwimUp,
    /// Walk to the nearest water block.
    ToWater { pos: BlockPos },
    /// Leave the fire block for `to`.
    OffFire { to: BlockPos },
    /// Eat the chosen food slot (an index into the inventory).
    Eat { slot: u16, item: ItemKind },
}

/// Per-tick memory the reflex functions read: visibility ages, the own-entity
/// facts, the last safe cell and the arrow headings.
#[derive(Clone, Debug)]
pub struct ReflexState {
    visible_since: [Option<u64>; 7],
    cooldown_until: [u64; 7],
    /// Air ticks left, as a fallback for when the server never sent the air
    /// supply (an older server, or the first ticks after a login). The simulated
    /// vanilla supply counts down while the head is in water; see
    /// [`ReflexState::air`] for why the fact wins when it exists.
    pub air_ticks: u32,
    /// The air supply the server sent for the player's own entity, in ticks.
    /// `None` means the fact is unknown, not that the player has full air.
    own_air: Option<u32>,
    /// Whether the player's own shared flags say it is on fire. `None` means
    /// the fact is unknown, not that the player is safe.
    own_burning: Option<bool>,
    /// The last cell where the player stood safely; `StepBack` returns here.
    pub last_safe_cell: Option<BlockPos>,
    pub arrows: ArrowMemory,
    /// The mob kind the running skill is fighting or walking to (its in-flight
    /// primitive is `attack` or `move_to` of that kind), without the
    /// `minecraft:` namespace. The arbiter sets it from the interpreter's
    /// decisions; `None` when the skill is engaged with no mob.
    engaged: Option<String>,
}

impl Default for ReflexState {
    fn default() -> Self {
        Self::new()
    }
}

impl ReflexState {
    pub fn new() -> Self {
        Self {
            visible_since: [None; 7],
            cooldown_until: [0; 7],
            air_ticks: FULL_AIR_TICKS,
            own_air: None,
            own_burning: None,
            last_safe_cell: None,
            arrows: ArrowMemory::default(),
            engaged: None,
        }
    }

    /// The running skill's engagement: the mob kind its in-flight `attack` or
    /// `move_to` names, or `None`. A namespace is dropped, so `zombie` and
    /// `minecraft:zombie` are the same engagement.
    pub fn set_engagement(&mut self, mob: Option<&str>) {
        self.engaged = mob.map(|name| strip_namespace(name).to_owned());
    }

    /// The mob kind the running skill is engaged with, if any.
    pub fn engagement(&self) -> Option<&str> {
        self.engaged.as_deref()
    }

    /// Whether the melee reflex leaves `mob` to the running skill: the skill is
    /// engaged with that kind and health is above `fight_min_health`. At or
    /// below it the reflex takes the body again, so a low-health flee is never
    /// held back by a skill (decision 2026-11-10 "The skill that started a fight
    /// owns it"). An unknown health is not "above".
    pub fn leaves_to_skill(&self, mob: EntityView, view: &View, settings: &Settings) -> bool {
        self.engaged
            .as_deref()
            .is_some_and(|engaged| engaged == strip_namespace(mob.kind.to_str()))
            && view
                .health
                .is_some_and(|health| health > settings.fight_min_health)
    }

    /// Record this tick's own-entity facts, as perception answered them.
    /// `None` keeps the fact unknown; it never turns a missing answer into a
    /// reassuring one.
    pub fn set_own_facts(&mut self, air: Option<u32>, burning: Option<bool>) {
        self.own_air = air;
        self.own_burning = burning;
    }

    /// Whether the air supply is a server fact rather than the simulation.
    pub fn knows_air(&self) -> bool {
        self.own_air.is_some()
    }

    /// Air ticks left: the server's fact when it exists, and the simulated
    /// vanilla supply only as the fallback while the fact is unknown.
    ///
    /// A desync (a reconnect, a lag spike) cannot make the reflex fire early or
    /// late once the fact arrives, which is why the fact wins; the simulation
    /// only keeps the reflex alive on a server that never sends the metadata.
    pub fn air(&self) -> u32 {
        self.own_air.unwrap_or(self.air_ticks)
    }

    /// Whether the body is burning: the server's shared flags when they exist,
    /// and `false` (the fire-block view alone decides then) while they do not.
    pub fn burning(&self) -> bool {
        self.own_burning.unwrap_or(false)
    }

    /// When the trigger was first visible, or `None` when it is not visible.
    pub fn visible_since(&self, kind: ReflexKind) -> Option<u64> {
        self.visible_since[kind.index()]
    }

    /// How long the current trigger has been visible, in ticks.
    pub fn visible_ticks(&self, kind: ReflexKind, tick: u64) -> u64 {
        self.visible_since(kind)
            .map_or(0, |since| tick.saturating_sub(since))
    }

    /// The human-floor gate: a `human_floor` reflex acts on the third tick of
    /// visibility, `lethal_fast` on the first.
    pub fn may_act(&self, kind: ReflexKind, class: LatencyClass, tick: u64) -> bool {
        match class {
            LatencyClass::LethalFast => self.visible_since(kind).is_some(),
            LatencyClass::HumanFloor => {
                self.visible_ticks(kind, tick) + 1 >= HUMAN_FLOOR_TICKS
            }
        }
    }

    pub fn on_cooldown(&self, kind: ReflexKind, tick: u64) -> bool {
        tick < self.cooldown_until[kind.index()]
    }

    pub fn set_cooldown(&mut self, kind: ReflexKind, until: u64) {
        self.cooldown_until[kind.index()] = until;
    }

    /// Fold this tick's view into the memory. Called by the engine before it
    /// evaluates, so the latency gate always counts real visibility.
    ///
    /// `blocks` is the perception seam: the last safe cell is only recorded
    /// when the cells around the body are *known* safe, never when a sighting is
    /// missing.
    pub fn observe(
        &mut self,
        tick: u64,
        view: &View,
        settings: &Settings,
        blocks: &mut dyn KnownBlocks,
    ) {
        if view.head_in_water {
            self.air_ticks = self.air_ticks.saturating_sub(1);
        } else {
            self.air_ticks = FULL_AIR_TICKS;
        }
        if view.is_safe_standing(blocks) {
            self.last_safe_cell = Some(view.feet);
        }
        // Compute every trigger against the old state, then store the ages.
        let mut triggered = [false; 7];
        for kind in ReflexKind::ALL {
            triggered[kind.index()] = kind.triggered(view, self, settings);
        }
        for (kind, present) in ReflexKind::ALL.into_iter().zip(triggered) {
            let slot = &mut self.visible_since[kind.index()];
            if present {
                if slot.is_none() {
                    *slot = Some(tick);
                }
            } else {
                *slot = None;
            }
        }
        self.arrows.update(&view.arrows);
    }
}

/// The pure evaluation of one reflex. `None` means the trigger is not present
/// or nothing can be done about it.
pub fn evaluate(
    kind: ReflexKind,
    view: &View,
    state: &ReflexState,
    settings: &Settings,
) -> Option<ReflexAction> {
    if !kind.triggered(view, state, settings) {
        return None;
    }
    action_for(kind, view, state, settings)
}

/// What a reflex of `kind` does about this view, without asking whether its
/// trigger is present. The engine uses it to renew a running reflex in place,
/// whose trigger is the wider [`ReflexKind::holds`] test.
pub fn action_for(
    kind: ReflexKind,
    view: &View,
    state: &ReflexState,
    settings: &Settings,
) -> Option<ReflexAction> {
    match kind {
        ReflexKind::Lava => {
            let fallback = away_from(view.feet, view.lava.as_ref().map(|lava| lava.pos));
            let to = state
                .last_safe_cell
                .filter(|cell| cell != &view.feet)
                .unwrap_or(fallback);
            Some(ReflexAction::StepBack { to })
        }
        ReflexKind::Drowning => Some(ReflexAction::SwimUp),
        ReflexKind::Fire => match &view.water {
            Some(water) if view.horizontal_to_block(water.pos) <= 8.0 => {
                Some(ReflexAction::ToWater { pos: water.pos })
            }
            _ => {
                let to = if view.next_cell != view.feet && !view.next_cell_lava {
                    view.next_cell
                } else {
                    away_from(view.feet, view.lava.as_ref().map(|lava| lava.pos))
                };
                Some(ReflexAction::OffFire { to })
            }
        },
        ReflexKind::Creeper => view.creeper.map(|creeper| ReflexAction::Flee {
            from: creeper.position,
            distance: settings.flee_distance,
        }),
        ReflexKind::Arrow => view.incoming_arrow().map(|arrow| ReflexAction::Strafe {
            right: arrow.strafe_right,
        }),
        ReflexKind::Melee => {
            let mob = melee_target(view, state, settings)?;
            let healthy = view.health.is_some_and(|health| health > settings.fight_min_health);
            // Unarmed, a flee only helps against a mob the sprinting body
            // outruns; against a spider or a skeleton it only turns the back
            // to it, so the body fights back (decision 2026-11-10 "Unarmed at
            // night: avoid contact"). Armed, low health still flees. Armed
            // means a sword, as in `night_watch` and the brain: an axe alone
            // is unarmed, though the body still hits with it.
            let armed = view.best_weapon.is_some_and(|weapon| weapon.is_sword());
            let fight = healthy || (!armed && !sprint_outruns(mob.kind));
            Some(if fight {
                ReflexAction::Attack {
                    target: mob.id,
                    weapon: view.best_weapon.map(|weapon| weapon.slot),
                }
            } else {
                ReflexAction::Flee {
                    from: mob.position,
                    distance: settings.flee_distance,
                }
            })
        }
        ReflexKind::Starvation => view.best_food.map(|food| ReflexAction::Eat {
            slot: food.slot,
            item: food.kind,
        }),
    }
}

/// The mob the melee reflex is about: the nearest tracked zombie, skeleton or
/// spider (horizontally) that is not left to the running skill. Only the
/// engaged kind is skipped, so a skill fighting a zombie never hides a spider
/// next to it from the reflex. The range test is the caller's: the trigger
/// and the exit range differ.
fn melee_target(view: &View, state: &ReflexState, settings: &Settings) -> Option<EntityView> {
    view.melee_mobs()
        .filter(|mob| !state.leaves_to_skill(*mob, view, settings))
        .min_by(|a, b| view.horizontal_to(*a).total_cmp(&view.horizontal_to(*b)))
}

/// Evaluate the whole set in priority order. Returns the first action whose
/// trigger is present; the engine applies the latency gate.
pub fn evaluate_all(
    view: &View,
    state: &ReflexState,
    settings: &Settings,
) -> Option<(ReflexKind, ReflexAction)> {
    /// Highest priority first; a const so the hot path allocates nothing.
    const ORDER: [ReflexKind; 7] = [
        ReflexKind::Lava,
        ReflexKind::Drowning,
        ReflexKind::Fire,
        ReflexKind::Creeper,
        ReflexKind::Arrow,
        ReflexKind::Melee,
        ReflexKind::Starvation,
    ];
    ORDER
        .into_iter()
        .find_map(|kind| evaluate(kind, view, state, settings).map(|action| (kind, action)))
}

/// A cardinal cell in the direction away from `threat`, as a best effort when
/// no remembered safe cell exists.
fn away_from(feet: BlockPos, threat: Option<BlockPos>) -> BlockPos {
    let Some(threat) = threat else {
        return feet.north(1);
    };
    let dx = feet.x - threat.x;
    let dz = feet.z - threat.z;
    if dx.abs() >= dz.abs() {
        feet.east(dx.signum())
    } else {
        feet.south(dz.signum())
    }
}

/// One log line for a reflex that started or renewed (decision 2026-11-10:
/// run 9 logged only `owner_changed reflex:melee`, never what the reflex did):
/// `reflex_action kind=<kind> action=<action> target=<id|-> distance=<blocks|-> renewed=<bool>`.
///
/// The target is the entity the action is about (the attacked or fled mob, the
/// dodged arrow); the distance is horizontal from the body to it, or to the
/// cell a step-back, a walk to water or a step off fire goes to.
pub fn action_note(kind: ReflexKind, action: ReflexAction, view: &View, renewed: bool) -> String {
    let mob = |from: Vec3| match kind {
        ReflexKind::Creeper => view.creeper,
        _ => view.melee_mobs().find(|mob| mob.position == from),
    };
    let (name, target, distance) = match action {
        ReflexAction::Attack { target, .. } => {
            let seen = view
                .melee_mob(target)
                .or_else(|| view.creeper.filter(|mob| mob.id == target));
            ("attack", Some(target), seen.map(|mob| view.horizontal_to(mob)))
        }
        ReflexAction::Flee { from, .. } => {
            let dx = view.position.x - from.x;
            let dz = view.position.z - from.z;
            ("flee", mob(from).map(|mob| mob.id), Some((dx * dx + dz * dz).sqrt()))
        }
        ReflexAction::Strafe { .. } => {
            let arrow = view.incoming_arrow();
            ("strafe", arrow.map(|arrow| arrow.id), arrow.map(|arrow| arrow.distance))
        }
        ReflexAction::StepBack { to } => ("step_back", None, Some(view.horizontal_to_block(to))),
        ReflexAction::SwimUp => ("swim_up", None, None),
        ReflexAction::ToWater { pos } => ("to_water", None, Some(view.horizontal_to_block(pos))),
        ReflexAction::OffFire { to } => ("off_fire", None, Some(view.horizontal_to_block(to))),
        ReflexAction::Eat { .. } => ("eat", None, None),
    };
    let target = target.map_or_else(|| "-".to_owned(), |id| id.to_string());
    let distance = distance.map_or_else(|| "-".to_owned(), |d| format!("{d:.2}"));
    format!(
        "reflex_action kind={} action={name} target={target} distance={distance} renewed={renewed}",
        kind.name()
    )
}

/// Whether a sprinting player outruns a mob of `kind` on open ground, so
/// fleeing it ends the fight. The one table of mob speed the reflexes use.
///
/// Vanilla `movement_speed` attribute values (26.3 mob attributes) are noted
/// for each kind; a player's own sprint is the walk attribute 0.1 times 1.3.
/// The two scales are not directly comparable (a mob's ground speed and a
/// player's input speed go through different code), so the classification
/// below is the decision's, not a computed one:
///
/// * zombie 0.23, husk 0.23, drowned 0.23 (on land): slower, flee works;
/// * spider 0.3: not outrun. TO BE MEASURED on the live server (decision
///   2026-11-10: "spider speed is to be measured");
/// * skeleton 0.25: a ranged mob, a flee in the open keeps it shooting.
///
/// Every other kind answers `false`: fight back rather than turn away.
pub fn sprint_outruns(kind: EntityKind) -> bool {
    matches!(kind, EntityKind::Zombie | EntityKind::Husk | EntityKind::Drowned)
}

/// `minecraft:zombie` and `zombie` name the same kind.
fn strip_namespace(name: &str) -> &str {
    name.strip_prefix("minecraft:").unwrap_or(name)
}

/// The entity kinds the melee reflex fights.
pub fn is_melee_kind(kind: EntityKind) -> bool {
    matches!(kind, EntityKind::Zombie | EntityKind::Skeleton | EntityKind::Spider)
}
