//! The small primitives: `eat`, `attack`, `equip`, `wait`, `observe`.

use azalea_core::position::Vec3;
use praxion_headless::{ControlFrame, MenuAction, MenuMode};
use praxion_interpreter::Outcome;
use praxion_planner::center;
use praxion_rules::{ArgValue, FactCall};

use crate::common::{
    HandSelect, Selecting, aim, block_target, count_item, entity_target, find_item, held_matches,
    hotbar_index, idle,
};
use crate::runner::{Ctx, IntentSpec};

#[derive(Debug)]
pub(crate) struct Eat {
    pub(crate) item: String,
    pub(crate) sent: bool,
    pub(crate) waited: u32,
    pub(crate) hand: HandSelect,
}

impl Eat {
    pub(crate) fn tick(
        &mut self,
        ctx: &mut Ctx<'_>,
        started: bool,
    ) -> (ControlFrame, Option<IntentSpec>, Option<Outcome>) {
        let mut frame = idle(ctx);
        // After the first Use the food stack can be empty while the receipt is
        // still in flight; only a before-use empty stack is `no_item`.
        if !self.sent && count_item(ctx.world, &self.item) == 0 {
            return (frame, None, Some(Outcome::Failed("no_item".into())));
        }
        if !self.sent {
            match self.hand.tick(ctx.world, &self.item, &mut frame) {
                Selecting::Held => {}
                Selecting::Pending => return (frame, None, None),
                Selecting::Unavailable => {
                    return (frame, None, Some(Outcome::Failed("no_item".into())));
                }
            }
        }
        self.waited += 1;
        if self.waited > 140 {
            return (frame, None, Some(Outcome::Failed("not_hungry".into())));
        }
        frame.clicks.push(praxion_headless::Click::Use);
        self.sent = true;
        let pending = if started {
            None
        } else {
            Some(IntentSpec::Inventory {
                primitive: "eat".to_owned(),
                item: self.item.clone(),
                delta: -1,
                window: 80,
            })
        };
        let _ = self.sent;
        (frame, pending, None)
    }
}

// ----------------------------------------------------------------------
// attack
// ----------------------------------------------------------------------

#[derive(Debug)]
pub(crate) struct Attack {
    pub(crate) kind: String,
    pub(crate) entity: Option<i32>,
    /// The locked mob's position, rewritten from the store every tick.
    pub(crate) target: Option<Vec3>,
    pub(crate) waited: u32,
    /// Ticks the locked mob has been out of reach. Same bound as the search.
    pub(crate) out_of_reach: u32,
    /// Brings the best weapon carried into the main hand, hotbar or one swap.
    pub(crate) hand: HandSelect,
    /// Ticks spent waiting for that swap before fighting as we are.
    pub(crate) weapon_waited: u32,
}

/// Ticks `attack` waits for a mob within reach before it fails `no_target`.
const NO_TARGET_TICKS: u32 = 10;

/// How far the eye may be from the mob's feet and still click it. The same
/// metric and radius as the `near_entity(mob, 3)` guard that selects `attack`,
/// so the primitive and the rule that fired it never disagree about reach.
const ATTACK_REACH: f64 = 3.0;

/// Ticks `attack` waits for a swap to put the best weapon in the hand before
/// it fights with what it holds. A swap shows only after the server's slot
/// update (`common::HandSelect`); a server that drops it must not freeze the
/// fight, so the wait is bounded.
const WEAPON_WAIT_TICKS: u32 = 20;

/// The best weapon the body carries, hotbar and main inventory (window slots 9
/// to 44), by `weapon_rank`. Ties prefer the hotbar, so a sword already in
/// reach is not swapped for its twin. This is the item name the hand select
/// brings up.
fn best_weapon(world: &praxion_world::World) -> Option<&str> {
    (9..=44usize)
        .filter_map(|slot| {
            let stack = world.inventory().get(slot)?;
            if stack.count() <= 0 {
                return None;
            }
            let rank = praxion_gamedata::weapon_rank(crate::common::item_name(stack))?;
            Some((rank, slot >= 36, crate::common::item_name(stack)))
        })
        .max_by_key(|(rank, hotbar, _)| (*rank, *hotbar))
        .map(|(_, _, name)| name)
}

impl Attack {
    pub(crate) fn tick(
        &mut self,
        ctx: &mut Ctx<'_>,
        started: bool,
    ) -> (ControlFrame, Option<IntentSpec>, Option<Outcome>) {
        let mut frame = idle(ctx);
        // Only the allowlisted mobs are ever attacked: a creeper or an
        // Enderman is refused before any approach (decision 2026-11-09 "Five
        // next areas"; the reflex and the look guard own those two).
        if !praxion_gamedata::huntable(&self.kind) {
            return (frame, None, Some(Outcome::Failed("not_huntable".into())));
        }
        // The locked mob's position is asked for again every tick: a mob that
        // walks away must not leave the body swinging at where it stood
        // (review finding 2). The read is by the id the primitive locked, on
        // the crate's one allowlisted store read; perception has no identity
        // query and would spend a ray for what is already ours.
        let current = self.entity.and_then(|id| {
            ctx.world
                .entity(id)
                .map(|entity| (entity.position(), entity.is_dying()))
        });
        if self.entity.is_some() {
            // Gone from the store, or dead and playing its death animation (the
            // server keeps a killed mob about 20 ticks before removing it, and
            // knockback can carry it out of reach meanwhile): the kill
            // registered either way.
            let Some((position, false)) = current else {
                return (frame, None, Some(Outcome::Confirmed));
            };
            if ctx.body.eye_position().distance_to(position) > ATTACK_REACH {
                // Out of reach: nothing to click. Give the mob the same ten
                // ticks the search allows, then hand control back so the
                // skill's next rule (`move_to`) walks to it. Looking at it
                // keeps the approach aligned.
                self.out_of_reach += 1;
                self.target = Some(position);
                frame.look = aim(ctx, Vec3::new(position.x, position.y + 1.0, position.z)).0;
                if self.out_of_reach > NO_TARGET_TICKS {
                    return (frame, None, Some(Outcome::Failed("no_target".into())));
                }
                let pending = if started {
                    None
                } else {
                    self.entity.map(|entity_id| IntentSpec::Attack {
                        entity_id,
                        window: 20,
                    })
                };
                return (frame, pending, None);
            }
            self.out_of_reach = 0;
            // The refreshed position is what the next swing aims at; a stale
            // one makes the body click the air behind the mob.
            self.target = Some(position);
        }
        if self.entity.is_none() {
            if let Some((id, position)) = entity_target(ctx, &self.kind, ATTACK_REACH as u32) {
                self.entity = Some(id);
                self.target = Some(position);
            } else {
                self.waited += 1;
                // A mob that stepped out of reach hands control back to the
                // skill (`hunt` moves to it again) instead of freezing the
                // body for two seconds.
                if self.waited > NO_TARGET_TICKS {
                    return (frame, None, Some(Outcome::Failed("no_target".into())));
                }
                return (frame, None, None);
            }
        }
        let Some(target) = self.target else {
            return (frame, None, Some(Outcome::Failed("no_target".into())));
        };
        let pending = if started {
            None
        } else {
            self.entity.map(|entity_id| IntentSpec::Attack {
                entity_id,
                window: 20,
            })
        };
        // The best weapon the body carries is brought into the main hand
        // (brief step 1: a weapon is equipped by hotbar select or menu swap;
        // review finding 8). `HandSelect` sends the swap once and waits for the
        // server's slot update instead of swapping again, which would undo it.
        // The fight waits at most `WEAPON_WAIT_TICKS` for that, then goes on
        // with whatever is held.
        if let Some(weapon) = best_weapon(ctx.world) {
            match self.hand.tick(ctx.world, weapon, &mut frame) {
                Selecting::Held => self.weapon_waited = 0,
                Selecting::Pending => {
                    self.weapon_waited += 1;
                    if self.weapon_waited <= WEAPON_WAIT_TICKS {
                        return (frame, pending, None);
                    }
                }
                Selecting::Unavailable => self.weapon_waited = 0,
            }
        }
        let aim_point = Vec3::new(target.x, target.y + 1.0, target.z);
        let (look, aligned) = aim(ctx, aim_point);
        frame.look = look;
        if aligned {
            frame.clicks.push(praxion_headless::Click::Attack);
        }
        self.waited += 1;
        if self.waited > 400 {
            return (frame, None, Some(Outcome::Failed("target_alive".into())));
        }
        (frame, pending, None)
    }
}

// ----------------------------------------------------------------------
// equip
// ----------------------------------------------------------------------

#[derive(Debug)]
pub(crate) struct Equip {
    pub(crate) item: String,
    pub(crate) stage: EquipStage,
    pub(crate) waited: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EquipStage {
    Select,
    Wait,
}

impl Equip {
    pub(crate) fn tick(
        &mut self,
        ctx: &mut Ctx<'_>,
    ) -> (ControlFrame, Option<IntentSpec>, Option<Outcome>) {
        let mut frame = idle(ctx);
        self.waited += 1;
        if hotbar_index(ctx.world, &self.item).is_some() {
            if held_matches(ctx.world, &self.item) {
                return (frame, None, Some(Outcome::Confirmed));
            }
            if let Some(slot) = hotbar_index(ctx.world, &self.item) {
                frame.select_slot = Some(slot);
            }
            return (frame, None, None);
        }
        if self.stage == EquipStage::Select {
            let Some((index, _)) = find_item(ctx.world, &self.item) else {
                return (frame, None, Some(Outcome::Failed("no_item".into())));
            };
            if !(9..=35).contains(&index) {
                return (frame, None, Some(Outcome::Failed("no_item".into())));
            }
            frame.menu = Some(MenuAction {
                window: 0,
                slot: index as i16,
                button: 0,
            });
            frame.menu_mode = MenuMode::Swap;
            self.stage = EquipStage::Wait;
            self.waited = 0;
            return (frame, None, None);
        }
        if self.waited > 40 {
            return (frame, None, Some(Outcome::Failed("no_item".into())));
        }
        if let Some(slot) = hotbar_index(ctx.world, &self.item) {
            frame.select_slot = Some(slot);
        }
        (frame, None, None)
    }
}

// ----------------------------------------------------------------------
// wait
// ----------------------------------------------------------------------

#[derive(Debug)]
pub(crate) struct Wait {
    pub(crate) remaining: u32,
    pub(crate) total: u32,
}

impl Wait {
    pub(crate) fn tick(
        &mut self,
        ctx: &mut Ctx<'_>,
    ) -> (ControlFrame, Option<IntentSpec>, Option<Outcome>) {
        let frame = idle(ctx);
        if self.remaining == 0 {
            return (frame, None, Some(Outcome::Confirmed));
        }
        self.remaining -= 1;
        let pending = if self.remaining + 1 == self.total {
            Some(IntentSpec::Movement {
                primitive: "wait".to_owned(),
                args: vec![self.total.to_string()],
                window: u64::from(self.total) + 20,
            })
        } else {
            None
        };
        (frame, pending, None)
    }
}

// ----------------------------------------------------------------------
// observe
// ----------------------------------------------------------------------

#[derive(Debug)]
pub(crate) struct Observe {
    pub(crate) fact: FactCall,
    pub(crate) target: Option<Vec3>,
    pub(crate) waited: u32,
}

impl Observe {
    pub(crate) fn tick(
        &mut self,
        ctx: &mut Ctx<'_>,
        started: bool,
    ) -> (ControlFrame, Option<IntentSpec>, Option<Outcome>) {
        let mut frame = idle(ctx);
        if fact_known(ctx, &self.fact) {
            let pending = if started {
                None
            } else {
                Some(IntentSpec::Fact {
                    fact: self.fact.clone(),
                    window: 40,
                })
            };
            return (frame, pending, Some(Outcome::Confirmed));
        }
        if self.target.is_none() {
            self.target = observe_target(ctx, &self.fact);
        }
        if let Some(target) = self.target {
            let (look, aligned) = aim(ctx, target);
            frame.look = look;
            if aligned {
                let pending = if started {
                    None
                } else {
                    Some(IntentSpec::Fact {
                        fact: self.fact.clone(),
                        window: 40,
                    })
                };
                return (frame, pending, None);
            }
        }
        self.waited += 1;
        if self.waited > 80 {
            return (frame, None, Some(Outcome::Failed("fact_unknown".into())));
        }
        (frame, None, None)
    }
}

fn observe_target(ctx: &mut Ctx<'_>, fact: &FactCall) -> Option<Vec3> {
    match fact.name.as_str() {
        "near" => {
            let name = fact.args.first()?.name()?;
            block_target(ctx, name, 16).map(center)
        }
        "near_entity" => {
            let name = fact.args.first()?.name()?;
            entity_target(ctx, name, 16).map(|(_, position)| position)
        }
        _ => Some(ctx.body.position()),
    }
}

pub fn fact_known(ctx: &mut Ctx<'_>, fact: &FactCall) -> bool {
    match fact.name.as_str() {
        "count" | "has" => true,
        "near" => {
            let Some(name) = fact.args.first().and_then(ArgValue::name) else {
                return false;
            };
            let radius = fact
                .args
                .get(1)
                .and_then(ArgValue::as_int)
                .unwrap_or(0)
                .max(0) as u32;
            block_target(ctx, name, radius).is_some()
        }
        "near_entity" => {
            let Some(name) = fact.args.first().and_then(ArgValue::name) else {
                return false;
            };
            let radius = fact
                .args
                .get(1)
                .and_then(ArgValue::as_int)
                .unwrap_or(0)
                .max(0) as u32;
            entity_target(ctx, name, radius).is_some()
        }
        _ => false,
    }
}

#[cfg(test)]
mod attack_tests {
    use super::*;
    use crate::test_world::{add_entity, feet, give, swept, world};
    use azalea_registry::builtin::{EntityKind, ItemKind};
    use praxion_headless::Body;
    use praxion_planner::Personality;

    fn attack_for(kind: &str) -> Attack {
        Attack {
            kind: kind.to_owned(),
            entity: None,
            target: None,
            waited: 0,
            out_of_reach: 0,
            hand: HandSelect::default(),
            weapon_waited: 0,
        }
    }

    fn tick(
        attack: &mut Attack,
        world: &praxion_world::World,
        perception: &mut praxion_perception::Perception,
        body: &mut Body,
        tick: u64,
    ) -> (ControlFrame, Option<IntentSpec>, Option<Outcome>) {
        let mut ctx = Ctx {
            tick,
            body,
            world,
            perception,
            style: Personality::default(),
        };
        let result = attack.tick(&mut ctx, false);
        result.0.apply_with_menu(body, tick).expect("frame applies");
        result
    }

    #[test]
    fn attack_gives_up_after_ten_ticks_without_a_mob_in_reach() {
        let world = world();
        let mut perception = swept(&world);
        let mut body = Body::new(feet());
        let mut attack = attack_for("zombie");
        let mut failed_at = None;
        for n in 0..40u64 {
            let (_, _, outcome) = tick(&mut attack, &world, &mut perception, &mut body, 20 + n);
            if let Some(outcome) = outcome {
                assert_eq!(outcome, Outcome::Failed("no_target".into()));
                failed_at = Some(n);
                break;
            }
        }
        assert_eq!(failed_at, Some(10), "the 11th tick fails, not the 41st");
    }

    #[test]
    fn attack_refuses_a_creeper_and_an_enderman_even_in_reach() {
        for kind in ["creeper", "enderman", "wolf"] {
            let mut world = world();
            add_entity(&mut world, 3, EntityKind::Creeper, Vec3::new(0.5, 64.0, 2.5));
            let mut perception = swept(&world);
            let mut body = Body::new(feet());
            let mut attack = attack_for(kind);
            let (frame, _, outcome) = tick(&mut attack, &world, &mut perception, &mut body, 20);
            assert_eq!(outcome, Some(Outcome::Failed("not_huntable".into())), "{kind}");
            assert!(frame.clicks.is_empty(), "no click at a {kind}");
        }
    }

    #[test]
    fn attack_holds_the_best_hotbar_weapon() {
        let mut world = world();
        give(&mut world, 37, ItemKind::WoodenSword, 1);
        give(&mut world, 38, ItemKind::DiamondSword, 1);
        give(&mut world, 39, ItemKind::StonePickaxe, 1);
        add_entity(&mut world, 3, EntityKind::Zombie, Vec3::new(0.5, 64.0, 2.5));
        let mut perception = swept(&world);
        let mut body = Body::new(feet());
        let mut attack = attack_for("zombie");
        let (frame, _, _) = tick(&mut attack, &world, &mut perception, &mut body, 20);
        assert_eq!(frame.select_slot, Some(2), "the diamond sword, hotbar 2");
    }

    #[test]
    fn attack_with_no_weapon_selects_nothing() {
        let mut world = world();
        give(&mut world, 38, ItemKind::StonePickaxe, 1);
        add_entity(&mut world, 3, EntityKind::Zombie, Vec3::new(0.5, 64.0, 2.5));
        let mut perception = swept(&world);
        let mut body = Body::new(feet());
        let mut attack = attack_for("zombie");
        let (frame, _, _) = tick(&mut attack, &world, &mut perception, &mut body, 20);
        assert_eq!(frame.select_slot, None);
    }

    #[test]
    fn a_locked_mob_in_its_death_animation_is_a_kill() {
        // Seen live on 2026-11-09: the server keeps a killed cow about 20 ticks
        // with the `Dying` pose, knockback carried it out of reach, and the call
        // failed `no_target` although the cow was dead.
        let mut world = world();
        add_entity(&mut world, 3, EntityKind::Zombie, Vec3::new(0.5, 64.0, 2.5));
        let mut perception = swept(&world);
        let mut body = Body::new(feet());
        let mut attack = attack_for("zombie");
        let (_, _, outcome) = tick(&mut attack, &world, &mut perception, &mut body, 20);
        assert!(outcome.is_none(), "the mob is locked");
        crate::test_world::set_dying(&mut world, 3);
        crate::test_world::teleport_entity(&mut world, 3, Vec3::new(0.5, 64.0, 9.5));
        let (frame, _, outcome) = tick(&mut attack, &world, &mut perception, &mut body, 21);
        assert_eq!(outcome, Some(Outcome::Confirmed));
        assert!(!frame.clicks.contains(&praxion_headless::Click::Attack), "no swing at a corpse");
    }

    #[test]
    fn attack_follows_the_mob_and_gives_up_when_it_leaves_reach() {
        let mut world = world();
        add_entity(&mut world, 3, EntityKind::Zombie, Vec3::new(0.5, 64.0, 2.5));
        let mut perception = swept(&world);
        let mut body = Body::new(feet());
        let mut attack = attack_for("zombie");
        let (_, _, outcome) = tick(&mut attack, &world, &mut perception, &mut body, 20);
        assert!(outcome.is_none());
        assert_eq!(
            attack.target,
            Some(Vec3::new(0.5, 64.0, 2.5)),
            "the mob is locked in reach"
        );

        // The mob walks seven blocks away. The primitive must aim at where it
        // is now, not at the cell it left, and must not swing at the empty
        // cell for the rest of the call (review finding 2).
        let moved = Vec3::new(0.5, 64.0, 9.5);
        crate::test_world::teleport_entity(&mut world, 3, moved);
        let mut failed_at = None;
        for n in 0..40u64 {
            let (frame, _, outcome) = tick(&mut attack, &world, &mut perception, &mut body, 21 + n);
            assert!(
                !frame.clicks.contains(&praxion_headless::Click::Attack),
                "no swing at a mob out of reach (tick {n})"
            );
            assert_eq!(attack.target, Some(moved), "the aim follows the mob (tick {n})");
            if let Some(outcome) = outcome {
                failed_at = Some((n, outcome));
                break;
            }
        }
        let (at, outcome) = failed_at.expect("an unreachable mob ends the call");
        assert_eq!(
            outcome,
            Outcome::Failed("no_target".into()),
            "the skill gets the body back so `move_to` can walk"
        );
        assert_eq!(
            at,
            u64::from(NO_TARGET_TICKS),
            "ten ticks of grace, then the skill walks"
        );
    }

    #[test]
    fn attack_swaps_the_best_carried_weapon_into_the_hand() {
        let mut world = world();
        // The sword sits in the main inventory, not the hotbar: `/give` may put
        // it there (review finding 10).
        give(&mut world, 20, ItemKind::DiamondSword, 1);
        add_entity(&mut world, 3, EntityKind::Zombie, Vec3::new(0.5, 64.0, 2.5));
        let mut perception = swept(&world);
        let mut body = Body::new(feet());
        let mut attack = attack_for("zombie");
        let (frame, _, _) = tick(&mut attack, &world, &mut perception, &mut body, 20);
        assert_eq!(
            frame.menu,
            Some(MenuAction {
                window: 0,
                slot: 20,
                button: 0,
            }),
            "one swap click puts the sword in hotbar 0"
        );
        assert_eq!(frame.menu_mode, MenuMode::Swap);
        assert!(
            frame.clicks.is_empty(),
            "the body waits for the sword instead of punching with a fist"
        );

        // The server has not echoed the swap yet: the call waits. A second
        // swap would swap the sword back out.
        let (frame, _, _) = tick(&mut attack, &world, &mut perception, &mut body, 21);
        assert_eq!(frame.menu, None, "never two swaps");
        assert!(frame.clicks.is_empty(), "still waiting");

        // The server echoes it: the sword is in hotbar 0, so it is in the main
        // hand and the fight goes on. The body turns over a tick or two, so the
        // first swing lands after the aim request.
        give(&mut world, 36, ItemKind::DiamondSword, 1);
        let mut swung = false;
        for n in 0..3u64 {
            let (frame, _, _) = tick(&mut attack, &world, &mut perception, &mut body, 22 + n);
            assert_eq!(frame.menu, None, "never two swaps");
            swung |= frame.clicks.contains(&praxion_headless::Click::Attack);
        }
        assert!(swung, "the sword is in hand and the body swings");
    }
}
