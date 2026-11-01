//! The in-memory fake world (test level 1).
//!
//! It implements the same two traits the real body will: [`Facts`] answers the
//! rule-list facts with three-valued results (a per-fact `unknown` mode tests
//! the `U` path), and [`Actuator`] starts primitives and reports their outcome
//! after a simulated duration. Inventory, nearby blocks and entities, the
//! first-night recipes, seeded drops and injected undo-perturbations live
//! here.
//!
//! The world is a handle (`Rc<RefCell<State>>`), so it can be passed as the
//! facts source and the actuator at the same time, and still be inspected
//! afterwards.

mod rng;
pub use praxion_gamedata as recipes;

#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use praxion_interpreter::{Actuator, Facts, IntValue, Outcome, StartAction, Truth};
use praxion_rules::{ArgValue, FactCall};

use rng::Rng;

/// Simulated duration of each discrete primitive, in ticks.
pub const MINE_TICKS: u64 = 6;
pub const PLACE_TICKS: u64 = 3;
pub const CRAFT_TICKS: u64 = 4;
pub const EAT_TICKS: u64 = 2;
pub const ATTACK_TICKS: u64 = 3;
pub const EQUIP_TICKS: u64 = 1;

const CHANCE_MINE_FAIL: f64 = 0.03;
const CHANCE_ATTACK_MISS: f64 = 0.05;
const CHANCE_PERTURB: f64 = 0.5;

/// Counters and costs the tests assert on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub actions_started: u32,
    pub actions_confirmed: u32,
    pub actions_failed: u32,
    pub continuous_applies: u32,
    pub placements: u32,
    /// A block was placed while a block of the same name was already nearby.
    pub duplicate_placements: u32,
    pub crafts: u32,
    /// A craft started while the item was already at its useful cap.
    pub wasted_crafts: u32,
    pub perturbations_fired: u32,
}

/// One injected setback, applied at a later matching primitive boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Perturbation {
    /// Remove one of the item right after a successful `craft(item)`.
    RemoveItemAfterCraft(String),
    /// Break the block right after a successful `place(block)`.
    BreakBlockAfterPlace(String),
}

/// One logged primitive boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub tick: u64,
    pub action: StartAction,
    pub outcome: Option<Outcome>,
}

#[derive(Clone, Debug)]
struct Nearby {
    name: String,
    distance: u32,
}

#[derive(Clone, Debug)]
struct Flight {
    action: StartAction,
    settle_at: u64,
    immediate: Option<Outcome>,
}

#[derive(Debug)]
struct State {
    seed: u64,
    rng: Rng,
    tick: u64,
    inventory: BTreeMap<String, u32>,
    blocks: Vec<Nearby>,
    entities: Vec<Nearby>,
    unknown: BTreeSet<String>,
    flight: Option<Flight>,
    stats: Stats,
    caps: BTreeMap<String, u32>,
    perturbations: Vec<Perturbation>,
    events: Vec<Event>,
    move_progress: BTreeMap<String, u32>,
}

impl State {
    fn count(&self, item: &str) -> u32 {
        self.inventory.get(item).copied().unwrap_or(0)
    }

    fn grant(&mut self, item: &str, count: u32) {
        *self.inventory.entry(item.to_string()).or_insert(0) += count;
    }

    /// Grants one random count in each declared drop's range. The fake world
    /// never invents a drop the primitive record does not declare.
    fn grant_drops(&mut self, drops: &[recipes::Drop]) {
        for drop in drops {
            let count = if drop.min >= drop.max {
                drop.min
            } else {
                self.rng.range(drop.min, drop.max)
            };
            if count > 0 {
                self.grant(drop.item, count);
            }
        }
    }

    fn take(&mut self, item: &str, count: u32) -> bool {
        let have = self.count(item);
        if have < count {
            return false;
        }
        let left = have - count;
        if left == 0 {
            self.inventory.remove(item);
        } else {
            self.inventory.insert(item.to_string(), left);
        }
        true
    }

    fn nearby(&self, name: &str, radius: u32) -> bool {
        self.blocks
            .iter()
            .any(|block| block.name == name && block.distance <= radius)
    }

    fn nearby_entity(&self, kind: &str, radius: u32) -> bool {
        self.entities
            .iter()
            .any(|entity| entity.name == kind && entity.distance <= radius)
    }

    fn remove_block(&mut self, name: &str) -> bool {
        if let Some(index) = self.blocks.iter().position(|block| block.name == name) {
            self.blocks.remove(index);
            true
        } else {
            false
        }
    }

    fn has_pickaxe(&self) -> bool {
        self.count("stone_pickaxe") > 0
            || self.count("wooden_pickaxe") > 0
            || self.count("iron_pickaxe") > 0
            || self.count("diamond_pickaxe") > 0
    }

    fn start(&mut self, action: &StartAction, tick: u64) -> u32 {
        self.stats.actions_started += 1;
        self.events.push(Event {
            tick,
            action: action.clone(),
            outcome: None,
        });
        let (duration, immediate) = self.precheck(action);
        self.flight = Some(Flight {
            action: action.clone(),
            settle_at: tick + duration,
            immediate,
        });
        duration as u32
    }

    /// Validates preconditions. A violation still takes the primitive's
    /// duration to report, like a real attempt that fails.
    fn precheck(&self, action: &StartAction) -> (u64, Option<Outcome>) {
        match action.primitive.as_str() {
            "mine" => {
                let Some(block) = arg_name(action, 0) else {
                    return (MINE_TICKS, Some(Outcome::Failed("bad_argument".into())));
                };
                if !self.nearby(block, 4) {
                    return (MINE_TICKS, Some(Outcome::Failed("no_block".into())));
                }
                if recipes::needs_pickaxe(block) && !self.has_pickaxe() {
                    return (MINE_TICKS, Some(Outcome::Failed("needs_tool".into())));
                }
                (MINE_TICKS, None)
            }
            "place" => {
                let Some(block) = arg_name(action, 0) else {
                    return (PLACE_TICKS, Some(Outcome::Failed("bad_argument".into())));
                };
                if self.count(block) == 0 {
                    return (PLACE_TICKS, Some(Outcome::Failed("no_item".into())));
                }
                (PLACE_TICKS, None)
            }
            "craft" => {
                let Some(item) = arg_name(action, 0) else {
                    return (CRAFT_TICKS, Some(Outcome::Failed("bad_argument".into())));
                };
                let Some(recipe) = recipes::recipe(item) else {
                    return (CRAFT_TICKS, Some(Outcome::Failed("no_recipe".into())));
                };
                if recipe
                    .inputs
                    .iter()
                    .any(|input| self.count(input.item) < input.count)
                {
                    return (CRAFT_TICKS, Some(Outcome::Failed("missing_ingredients".into())));
                }
                if recipe.needs_table && !self.nearby("crafting_table", 4) {
                    return (CRAFT_TICKS, Some(Outcome::Failed("needs_table".into())));
                }
                (CRAFT_TICKS, None)
            }
            "eat" | "equip" => {
                let Some(item) = arg_name(action, 0) else {
                    return (EAT_TICKS, Some(Outcome::Failed("bad_argument".into())));
                };
                if self.count(item) == 0 {
                    let duration = if action.primitive == "eat" {
                        EAT_TICKS
                    } else {
                        EQUIP_TICKS
                    };
                    return (duration, Some(Outcome::Failed("no_item".into())));
                }
                let duration = if action.primitive == "eat" {
                    EAT_TICKS
                } else {
                    EQUIP_TICKS
                };
                (duration, None)
            }
            "attack" => {
                let Some(kind) = arg_name(action, 0) else {
                    return (ATTACK_TICKS, Some(Outcome::Failed("bad_argument".into())));
                };
                if !self.nearby_entity(kind, 5) {
                    return (ATTACK_TICKS, Some(Outcome::Failed("no_target".into())));
                }
                (ATTACK_TICKS, None)
            }
            "wait" | "move_to" | "observe" => (1, None),
            other => (1, Some(Outcome::Failed(format!("unknown_primitive:{other}")))),
        }
    }

    fn poll(&mut self, tick: u64) -> Option<Outcome> {
        if !self
            .flight
            .as_ref()
            .is_some_and(|flight| tick >= flight.settle_at)
        {
            return None;
        }
        let flight = self.flight.take().expect("checked above");
        self.tick = tick;
        let outcome = match flight.immediate {
            Some(outcome) => outcome,
            None => self.settle(&flight.action),
        };
        match outcome {
            Outcome::Confirmed => self.stats.actions_confirmed += 1,
            _ => self.stats.actions_failed += 1,
        }
        self.events.push(Event {
            tick,
            action: flight.action,
            outcome: Some(outcome.clone()),
        });
        Some(outcome)
    }

    fn settle(&mut self, action: &StartAction) -> Outcome {
        match action.primitive.as_str() {
            "mine" => {
                let block = arg_name(action, 0).unwrap_or_default().to_string();
                if self.rng.chance(CHANCE_MINE_FAIL) {
                    return Outcome::Failed("block_resisted".into());
                }
                self.remove_block(&block);
                if let Some(drops) = recipes::block_drops(&block) {
                    self.grant_drops(drops);
                }
                Outcome::Confirmed
            }
            "place" => {
                let block = arg_name(action, 0).unwrap_or_default().to_string();
                if !self.take(&block, 1) {
                    return Outcome::Failed("no_item".into());
                }
                if self.blocks.iter().any(|nearby| nearby.name == block) {
                    self.stats.duplicate_placements += 1;
                }
                self.blocks.push(Nearby {
                    name: block.clone(),
                    distance: 3,
                });
                self.stats.placements += 1;
                self.perturb_break(&block);
                Outcome::Confirmed
            }
            "craft" => {
                let item = arg_name(action, 0).unwrap_or_default().to_string();
                let Some(recipe) = recipes::recipe(&item) else {
                    return Outcome::Failed("no_recipe".into());
                };
                let cap = self.caps.get(&item).copied().unwrap_or(u32::MAX);
                if self.count(&item) >= cap {
                    self.stats.wasted_crafts += 1;
                }
                for input in recipe.inputs {
                    if !self.take(input.item, input.count) {
                        return Outcome::Failed("missing_ingredients".into());
                    }
                }
                self.grant(recipe.output, recipe.output_count);
                self.stats.crafts += 1;
                self.perturb_craft(&item);
                Outcome::Confirmed
            }
            "eat" => {
                let item = arg_name(action, 0).unwrap_or_default();
                if self.take(item, 1) {
                    Outcome::Confirmed
                } else {
                    Outcome::Failed("no_item".into())
                }
            }
            "equip" => Outcome::Confirmed,
            "attack" => {
                if self.rng.chance(CHANCE_ATTACK_MISS) {
                    return Outcome::Failed("missed".into());
                }
                let kind = arg_name(action, 0).unwrap_or_default();
                if let Some(drops) = recipes::entity_drops(kind) {
                    self.grant_drops(drops);
                }
                Outcome::Confirmed
            }
            "observe" => {
                if let Some(ArgValue::Fact(call)) = action.args.first() {
                    self.unknown.remove(&call.name);
                }
                Outcome::Confirmed
            }
            "wait" | "move_to" => Outcome::Confirmed,
            other => Outcome::Failed(format!("unknown_primitive:{other}")),
        }
    }

    fn perturb_craft(&mut self, item: &str) {
        let Some(index) = self.perturbations.iter().position(
            |perturbation| matches!(perturbation, Perturbation::RemoveItemAfterCraft(name) if name == item),
        ) else {
            return;
        };
        if self.rng.chance(CHANCE_PERTURB) {
            self.take(item, 1);
            self.perturbations.remove(index);
            self.stats.perturbations_fired += 1;
        }
    }

    fn perturb_break(&mut self, block: &str) {
        let Some(index) = self.perturbations.iter().position(
            |perturbation| matches!(perturbation, Perturbation::BreakBlockAfterPlace(name) if name == block),
        ) else {
            return;
        };
        if self.rng.chance(CHANCE_PERTURB) {
            self.remove_block(block);
            self.perturbations.remove(index);
            self.stats.perturbations_fired += 1;
        }
    }

    fn apply(&mut self, action: &StartAction, tick: u64) {
        self.stats.continuous_applies += 1;
        self.tick = tick;
        match action.primitive.as_str() {
            "move_to" => {
                if let Some(target) = arg_name(action, 0) {
                    let progress = self.move_progress.entry(target.to_string()).or_insert(0);
                    *progress += 1;
                    if *progress >= 10 {
                        self.approach(target);
                    }
                }
            }
            _ => {}
        }
        self.events.push(Event {
            tick,
            action: action.clone(),
            outcome: None,
        });
    }

    /// A continuous `move_to` brings the target within reach after ten ticks.
    fn approach(&mut self, target: &str) {
        for block in &mut self.blocks {
            if block.name == target {
                block.distance = block.distance.min(2);
            }
        }
        for entity in &mut self.entities {
            if entity.name == target {
                entity.distance = entity.distance.min(2);
            }
        }
    }
}

/// The fake world handle.
#[derive(Clone)]
pub struct FakeWorld {
    state: Rc<RefCell<State>>,
}

impl FakeWorld {
    /// A standard first-night scenario: stone and oak trees nearby, a few
    /// animals, an empty inventory.
    pub fn new(seed: u64) -> Self {
        let mut rng = Rng::new(seed);
        let stones = rng.range(24, 40);
        let logs = rng.range(12, 20);
        let cows = rng.range(1, 2);
        let mut blocks = Vec::new();
        for _ in 0..stones {
            blocks.push(Nearby {
                name: "stone".into(),
                distance: 3,
            });
        }
        for _ in 0..logs {
            blocks.push(Nearby {
                name: "oak_log".into(),
                distance: 3,
            });
        }
        let mut entities = Vec::new();
        for _ in 0..cows {
            entities.push(Nearby {
                name: "cow".into(),
                distance: 4,
            });
        }
        Self {
            state: Rc::new(RefCell::new(State {
                seed,
                rng,
                tick: 0,
                inventory: BTreeMap::new(),
                blocks,
                entities,
                unknown: BTreeSet::new(),
                flight: None,
                stats: Stats::default(),
                caps: default_caps(),
                perturbations: Vec::new(),
                events: Vec::new(),
                move_progress: BTreeMap::new(),
            })),
        }
    }

    /// An empty world: no blocks, no entities, an empty inventory.
    pub fn empty(seed: u64) -> Self {
        let world = Self::new(seed);
        {
            let mut state = world.state.borrow_mut();
            state.blocks.clear();
            state.entities.clear();
            state.inventory.clear();
        }
        world
    }

    pub fn seed(&self) -> u64 {
        self.state.borrow().seed
    }

    pub fn tick(&self) -> u64 {
        self.state.borrow().tick
    }

    pub fn count(&self, item: &str) -> u32 {
        self.state.borrow().count(item)
    }

    pub fn has(&self, item: &str) -> bool {
        self.count(item) > 0
    }

    pub fn inventory(&self) -> BTreeMap<String, u32> {
        self.state.borrow().inventory.clone()
    }

    pub fn add_item(&self, item: &str, count: u32) {
        self.state.borrow_mut().grant(item, count);
    }

    pub fn remove_item(&self, item: &str, count: u32) -> bool {
        self.state.borrow_mut().take(item, count)
    }

    pub fn add_block(&self, name: &str, distance: u32) {
        self.state.borrow_mut().blocks.push(Nearby {
            name: name.to_string(),
            distance,
        });
    }

    pub fn break_block(&self, name: &str) -> bool {
        self.state.borrow_mut().remove_block(name)
    }

    pub fn block_distance(&self, name: &str) -> Option<u32> {
        self.state
            .borrow()
            .blocks
            .iter()
            .filter(|block| block.name == name)
            .map(|block| block.distance)
            .min()
    }

    pub fn add_entity(&self, kind: &str, distance: u32) {
        self.state.borrow_mut().entities.push(Nearby {
            name: kind.to_string(),
            distance,
        });
    }

    /// Forces this fact to answer `Unknown`, or clears the mode.
    pub fn set_unknown(&self, fact: &str, on: bool) {
        let mut state = self.state.borrow_mut();
        if on {
            state.unknown.insert(fact.to_string());
        } else {
            state.unknown.remove(fact);
        }
    }

    pub fn is_unknown(&self, fact: &str) -> bool {
        self.state.borrow().unknown.contains(fact)
    }

    pub fn add_perturbation(&self, perturbation: Perturbation) {
        self.state.borrow_mut().perturbations.push(perturbation);
    }

    /// Sets the count above which a craft of `item` counts as wasted.
    pub fn set_craft_cap(&self, item: &str, cap: u32) {
        self.state
            .borrow_mut()
            .caps
            .insert(item.to_string(), cap);
    }

    pub fn stats(&self) -> Stats {
        self.state.borrow().stats.clone()
    }

    pub fn events(&self) -> Vec<Event> {
        self.state.borrow().events.clone()
    }
}

fn default_caps() -> BTreeMap<String, u32> {
    let mut caps = BTreeMap::new();
    caps.insert("oak_planks".to_string(), 12);
    caps.insert("stick".to_string(), 4);
    caps.insert("crafting_table".to_string(), 1);
    caps.insert("wooden_pickaxe".to_string(), 1);
    caps.insert("stone_pickaxe".to_string(), 1);
    caps.insert("stone_sword".to_string(), 1);
    caps
}

fn arg_name(action: &StartAction, index: usize) -> Option<&str> {
    action.args.get(index).and_then(ArgValue::name)
}

impl Facts for FakeWorld {
    fn bool_fact(&mut self, call: &FactCall) -> Truth {
        let state = self.state.borrow();
        if state.unknown.contains(&call.name) {
            return Truth::Unknown(call.clone());
        }
        let value = match call.name.as_str() {
            "has" => call
                .args
                .first()
                .and_then(ArgValue::name)
                .is_some_and(|name| state.count(name) > 0),
            "near" => match call.args.first().and_then(ArgValue::name) {
                Some(name) => {
                    let radius = call
                        .args
                        .get(1)
                        .and_then(ArgValue::as_int)
                        .unwrap_or(0)
                        .max(0) as u32;
                    state.nearby(name, radius)
                }
                None => false,
            },
            "near_entity" => match call.args.first().and_then(ArgValue::name) {
                Some(kind) => {
                    let radius = call
                        .args
                        .get(1)
                        .and_then(ArgValue::as_int)
                        .unwrap_or(0)
                        .max(0) as u32;
                    state.nearby_entity(kind, radius)
                }
                None => false,
            },
            _ => return Truth::Unknown(call.clone()),
        };
        if value {
            Truth::True
        } else {
            Truth::False
        }
    }

    fn int_fact(&mut self, call: &FactCall) -> IntValue {
        let state = self.state.borrow();
        if state.unknown.contains(&call.name) {
            return IntValue::Unknown(call.clone());
        }
        match call.name.as_str() {
            "count" => IntValue::Known(
                call.args
                    .first()
                    .and_then(ArgValue::name)
                    .map(|name| i64::from(state.count(name)))
                    .unwrap_or(0),
            ),
            _ => IntValue::Unknown(call.clone()),
        }
    }
}

impl Actuator for FakeWorld {
    fn start(&mut self, action: &StartAction, tick: u64) -> u32 {
        self.state.borrow_mut().start(action, tick)
    }

    fn poll(&mut self, tick: u64) -> Option<Outcome> {
        self.state.borrow_mut().poll(tick)
    }

    fn cancel(&mut self) -> Option<Outcome> {
        let mut state = self.state.borrow_mut();
        let flight = state.flight.take()?;
        let tick = state.tick;
        state.events.push(Event {
            tick,
            action: flight.action,
            outcome: Some(Outcome::Preempted),
        });
        Some(Outcome::Preempted)
    }

    fn apply(&mut self, action: &StartAction, tick: u64) {
        self.state.borrow_mut().apply(action, tick);
    }

    fn release(&mut self) {}
}

