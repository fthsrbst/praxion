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
    pub(crate) target: Option<Vec3>,
    pub(crate) waited: u32,
}

/// Ticks `attack` waits for a mob within reach before it fails `no_target`.
const NO_TARGET_TICKS: u32 = 10;

/// The hotbar index (0 to 8) of the best weapon held there, if any.
fn best_hotbar_weapon(world: &praxion_world::World) -> Option<u8> {
    (36..=44usize)
        .filter_map(|slot| {
            let stack = world.inventory().get(slot)?;
            let rank = praxion_gamedata::weapon_rank(crate::common::item_name(stack))?;
            (stack.count() > 0).then_some((rank, (slot - 36) as u8))
        })
        .max_by_key(|(rank, _)| *rank)
        .map(|(_, index)| index)
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
        if let Some(id) = self.entity
            && ctx.world.entity(id).is_none()
        {
            return (frame, None, Some(Outcome::Confirmed));
        }
        if self.entity.is_none() {
            if let Some((id, position)) = entity_target(ctx, &self.kind, 3) {
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
        // The best weapon in the hotbar is in hand while fighting (decision
        // 2026-11-09 "Five next areas"); the actuator sends a select only when
        // the slot changes. A weapon outside the hotbar is not selected here.
        frame.select_slot = best_hotbar_weapon(ctx.world);
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
        let pending = if started {
            None
        } else {
            self.entity.map(|entity_id| IntentSpec::Attack {
                entity_id,
                window: 20,
            })
        };
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
}
