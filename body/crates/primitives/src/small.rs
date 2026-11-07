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

impl Attack {
    pub(crate) fn tick(
        &mut self,
        ctx: &mut Ctx<'_>,
        started: bool,
    ) -> (ControlFrame, Option<IntentSpec>, Option<Outcome>) {
        let mut frame = idle(ctx);
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
                if self.waited > 40 {
                    return (frame, None, Some(Outcome::Failed("no_target".into())));
                }
                return (frame, None, None);
            }
        }
        let Some(target) = self.target else {
            return (frame, None, Some(Outcome::Failed("no_target".into())));
        };
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
