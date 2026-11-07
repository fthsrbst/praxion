//! The interpreter's facts over perception (Demo-0 step 10a).
//!
//! Every fact the rules registry declares is answered here from the perception
//! API and the raw store's own F3 facts (inventory, health, food, air, burning,
//! time). The rules are three-valued (docs/architecture.md §6.1):
//!
//! - a `Known` answer becomes `True`/`False` or `Known(n)`;
//! - an `Unknown` perception answer stays `Unknown(fact)` and is **never**
//!   turned into `False`. A cell nobody saw is not "no block"; an inventory the
//!   server never sent is not "empty".
//!
//! `count` and `has` read the player's own inventory, which is an F3 fact. The
//! block facts go through [`Perception`], so they spend rays and can answer
//! `Unknown(budget)`. The day-time facts come from the server's world clock.
//!
//! Facts the registry may add later answer `Unknown` until they are implemented
//! here; this crate is the one place the mapping lives.

use azalea_registry::builtin::{BlockKind, EntityKind};
use praxion_interpreter::{Facts as InterpreterFacts, IntValue, Truth};
use praxion_perception::{Answer, Caller, Perception};
use praxion_rules::{ArgValue, FactCall};
use praxion_world::{ItemStack, World};

#[cfg(test)]
mod tests;

/// One day of the overworld clock, in ticks.
pub const DAY_TICKS: u64 = 24_000;
/// First tick of Minecraft's night by sky darkness (the sun has set).
pub const NIGHT_START: u64 = 13_000;
/// First tick of morning (the sun rises again).
pub const NIGHT_END: u64 = 23_000;

/// The interpreter's `Facts` over one tick's perception and world.
pub struct Facts<'a> {
    pub perception: &'a mut Perception,
    pub world: &'a World,
}

impl Facts<'_> {
    pub fn new<'a>(perception: &'a mut Perception, world: &'a World) -> Facts<'a> {
        Facts { perception, world }
    }
}

impl InterpreterFacts for Facts<'_> {
    fn bool_fact(&mut self, call: &FactCall) -> Truth {
        bool_fact(self.perception, self.world, call)
    }

    fn int_fact(&mut self, call: &FactCall) -> IntValue {
        int_fact(self.perception, self.world, call)
    }
}

/// The boolean half of the fact mapping.
pub fn bool_fact(perception: &mut Perception, world: &World, call: &FactCall) -> Truth {
    match call.name.as_str() {
        "has" => match first_name(call) {
            Some(wanted) => match item_count(perception, world, wanted) {
                Some(count) => truth(count > 0),
                None => Truth::Unknown(call.clone()),
            },
            None => Truth::Unknown(call.clone()),
        },
        "near" => match near_block(perception, world, call) {
            Some(found) => truth(found),
            None => Truth::Unknown(call.clone()),
        },
        "near_entity" => match near_entity(perception, world, call) {
            Some(found) => truth(found),
            None => Truth::Unknown(call.clone()),
        },
        // Extra facts used by the first-night skills; they are not in the
        // registry yet, but answering them costs nothing and keeps skill
        // authors from needing a second facts source later.
        "health" => match perception.own_health(world).value() {
            Some(health) => truth(health.health() > 0.0),
            None => Truth::Unknown(call.clone()),
        },
        "food" => match perception.own_food(world).value() {
            Some(food) => truth(*food > 0),
            None => Truth::Unknown(call.clone()),
        },
        "air" => match perception.air(world).value() {
            Some(air) => truth(*air > 0),
            None => Truth::Unknown(call.clone()),
        },
        "burning" => match perception.burning(world).value() {
            Some(burning) => truth(*burning),
            None => Truth::Unknown(call.clone()),
        },
        "is_night" => match perception.day_time(world).value() {
            Some(day_time) => truth(is_night(*day_time)),
            None => Truth::Unknown(call.clone()),
        },
        _ => Truth::Unknown(call.clone()),
    }
}

/// The integer half of the fact mapping.
pub fn int_fact(perception: &mut Perception, world: &World, call: &FactCall) -> IntValue {
    match call.name.as_str() {
        "count" => match first_name(call) {
            Some(wanted) => match item_count(perception, world, wanted) {
                Some(count) => IntValue::Known(i64::from(count)),
                None => IntValue::Unknown(call.clone()),
            },
            None => IntValue::Unknown(call.clone()),
        },
        "health" => match perception.own_health(world).value() {
            Some(health) => IntValue::Known(i64::from(health.health().floor() as i32)),
            None => IntValue::Unknown(call.clone()),
        },
        "food" => match perception.own_food(world).value() {
            Some(food) => IntValue::Known(i64::from(*food)),
            None => IntValue::Unknown(call.clone()),
        },
        "air" => match perception.air(world).value() {
            Some(air) => IntValue::Known(i64::from(*air)),
            None => IntValue::Unknown(call.clone()),
        },
        "time_of_day" => match perception.day_time(world).value() {
            Some(day_time) => IntValue::Known(time_of_day(*day_time)),
            None => IntValue::Unknown(call.clone()),
        },
        "day_time" | "time" => match perception.day_time(world).value() {
            Some(day_time) => IntValue::Known(saturating_i64(*day_time)),
            None => IntValue::Unknown(call.clone()),
        },
        _ => IntValue::Unknown(call.clone()),
    }
}

/// The count of an item or an item tag (`#logs`) in the inventory, or `None`
/// when perception could not answer.
pub fn item_count(perception: &mut Perception, world: &World, wanted: &str) -> Option<u32> {
    let Answer::Known { value: inventory, .. } = perception.inventory(world) else {
        return None;
    };
    // Slot 0 is the 2x2 grid's result: a preview the server computes, not an
    // owned item. A reflex that cut a table craft short left the table shown
    // there; `has(crafting_table)` was true and every `place` failed `no_item`.
    Some(count_in(inventory.get(1..).unwrap_or_default(), wanted))
}

/// The count of an item or tag in a known inventory.
pub fn count_in(inventory: &[ItemStack], wanted: &str) -> u32 {
    inventory
        .iter()
        .filter(|stack| item_matches(stack, wanted))
        .map(|stack| stack.count().max(0) as u32)
        .sum()
}

/// Whether an item or tag is in the inventory, or `None` when unknown.
pub fn has_item(perception: &mut Perception, world: &World, wanted: &str) -> Option<bool> {
    item_count(perception, world, wanted).map(|count| count > 0)
}

/// A block or block tag within `radius`, or `None` when the eye cell is not
/// known yet.
///
/// The answer comes from the perception known map, exactly like `explore`'s
/// completion check (docs/rule-lists.md): a target is near when a cell this
/// perception has seen inside `radius` holds it. It spends no rays, and it
/// answers `Some(false)` when the map knows of none, so `not near(T, R)` can
/// start an exploration. A ray-based answer would disagree with `explore` when
/// a remembered cell is out of sight, which made a skill restart an explore
/// that finished instantly, forever.
pub fn near_block(
    perception: &mut Perception,
    world: &World,
    call: &FactCall,
) -> Option<bool> {
    let wanted = first_name(call)?;
    let radius = radius_arg(call)?;
    perception.eye_cell(world)?;
    let tag = wanted.strip_prefix('#');
    let wanted_kind = if tag.is_none() {
        Some(wanted.parse::<BlockKind>().ok()?)
    } else {
        None
    };
    let found = perception.nearest_known(world, radius, |_| false, |state| {
        let kind = BlockKind::from(state);
        match (tag, wanted_kind) {
            (Some(tag), _) => praxion_gamedata::block_in_tag(kind.to_str(), tag),
            (None, Some(wanted_kind)) => kind == wanted_kind,
            (None, None) => false,
        }
    });
    Some(found.is_some())
}

/// An entity kind within `radius`, or `None` when perception cannot say.
pub fn near_entity(
    perception: &mut Perception,
    world: &World,
    call: &FactCall,
) -> Option<bool> {
    let name = first_name(call)?;
    let radius = radius_arg(call)?;
    let kind = name.parse::<EntityKind>().ok()?;
    match perception.nearest_entity(Caller::Other, world, kind, radius) {
        Answer::Known { .. } => Some(true),
        Answer::Unknown(_) => None,
    }
}

/// Minecraft night, by the overworld clock (the sky darkens at 12542 and
/// brightens at 23460; 13000 to 23000 is the conservative core).
pub fn is_night(day_time: u64) -> bool {
    let day = day_time % DAY_TICKS;
    (NIGHT_START..NIGHT_END).contains(&day)
}

/// The time of day, 0 to 23999.
pub fn time_of_day(day_time: u64) -> i64 {
    (day_time % DAY_TICKS) as i64
}

fn first_name(call: &FactCall) -> Option<&str> {
    call.args.first().and_then(ArgValue::name)
}

fn radius_arg(call: &FactCall) -> Option<u32> {
    let radius = call.args.get(1).and_then(ArgValue::as_int)?;
    Some(radius.max(0) as u32)
}

fn truth(value: bool) -> Truth {
    if value { Truth::True } else { Truth::False }
}

fn item_matches(stack: &ItemStack, wanted: &str) -> bool {
    let name = stack.kind().to_str();
    if let Some(tag) = wanted.strip_prefix('#') {
        praxion_gamedata::item_in_tag(name, tag)
    } else {
        normalize(name) == normalize(wanted)
    }
}

fn normalize(name: &str) -> &str {
    name.strip_prefix("minecraft:").unwrap_or(name)
}

fn saturating_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}
