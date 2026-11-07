//! Perception-driven primitives: one state machine per primitive, one
//! `ControlFrame` per tick.
//!
//! A primitive reads the world through [`Perception`] (and the planner for
//! `move_to`), never through the raw store. It registers its intent with the
//! receipts engine at the first tick it acts; the driver does that after the
//! actuator reports the prediction sequence number or state id. The primitive
//! ends with the receipt's outcome, or with its own typed failure.

use azalea_core::position::BlockPos;
use praxion_headless::{Body, ControlFrame};
use praxion_interpreter::{Outcome, StartAction};
use praxion_perception::Perception;
use praxion_planner::Personality;
use praxion_rules::{ArgValue, FactCall};
use praxion_world::World;

use crate::common::{HandSelect, idle};
use crate::craft::Craft;
use crate::explore::{Explore, ExploreMemory};
use crate::mine::Mine;
use crate::move_to::Move;
use crate::place::Place;
use crate::small::{Attack, Eat, Equip, EquipStage, Observe, Wait};

/// One tick of the primitive's world.
pub struct Ctx<'a> {
    pub tick: u64,
    pub body: &'a Body,
    pub world: &'a World,
    pub perception: &'a mut Perception,
    /// The body's movement personality, so a follower it starts walks the same
    /// way as the driver's gaze.
    pub style: Personality,
}

/// What the driver registers with the receipts engine after the frame is
/// applied. The actuator's report supplies the sequence number or state id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IntentSpec {
    Dig {
        pos: BlockPos,
        window: u64,
    },
    Place {
        pos: BlockPos,
        block: String,
        window: u64,
    },
    Inventory {
        primitive: String,
        item: String,
        delta: i64,
        window: u64,
    },
    /// `craft`: the output must reach the player's storage.
    StorageInventory {
        primitive: String,
        item: String,
        delta: i64,
        window: u64,
    },
    Movement {
        primitive: String,
        /// Human-readable arguments, for the intent log.
        args: Vec<String>,
        window: u64,
    },
    SlotClick {
        container_id: i32,
        slot: i16,
        window: u64,
    },
    Attack {
        entity_id: i32,
        window: u64,
    },
    Fact {
        fact: FactCall,
        window: u64,
    },
}

/// One tick's output.
#[derive(Clone, Debug)]
pub struct TickResult {
    pub frame: ControlFrame,
    /// Register this intent if the actuator reported the key it needs.
    pub pending: Option<IntentSpec>,
    pub outcome: Option<Outcome>,
    /// Bounded diagnostics for the driver's event log.
    pub events: Vec<PrimitiveEvent>,
}

/// A primitive transition the driver writes to the event log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrimitiveEvent {
    /// The frontier `explore` chose to walk to.
    ExploreFrontier {
        cell: BlockPos,
        score: i64,
        candidates: u32,
    },
}

/// The primitive runner. It owns at most one active primitive.
#[derive(Debug, Default)]
pub struct Runner {
    active: Option<Active>,
    receipt: Option<Outcome>,
    outcome: Option<Outcome>,
    intent_id: Option<u64>,
    started: bool,
    /// The memory this runner's `explore` calls share. It is per runner, so
    /// two runners (parallel tests, two bodies) never see each other's
    /// history.
    explore_memory: ExploreMemory,
}

#[derive(Debug)]
enum Active {
    Mine(Mine),
    Place(Place),
    Craft(Craft),
    Eat(Eat),
    Explore(Explore),
    Attack(Attack),
    Equip(Equip),
    Wait(Wait),
    Observe(Observe),
    Move(Move),
}

impl Active {
    /// Whether this primitive keeps running after a receipt settles as
    /// `outcome`. A confirmed discrete primitive finishes; `attack` keeps
    /// going until its target is gone, a `craft` first closes its open
    /// crafting table, and `move_to` treats "unrefuted" as "not corrected",
    /// not as "arrived": the follower's own goal check ends it.
    ///
    /// `craft` is the one exception to the confirmed gate: it enters its close
    /// path on any receipt, so `Runner::settle` keeps it active; `Runner::tick`
    /// still ends a failed receipt before that path runs, as it always did.
    fn continues_after(&self, outcome: &Outcome) -> bool {
        match self {
            Active::Craft(craft) => craft.close_outcome.is_some(),
            Active::Mine(mine) => *outcome == Outcome::Confirmed && mine.pickup.is_some(),
            Active::Move(_) | Active::Explore(_) | Active::Attack(_) => {
                *outcome == Outcome::Confirmed
            }
            _ => false,
        }
    }
}

impl Runner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts one primitive. Returns a typed error for an unknown primitive or
    /// a malformed argument.
    pub fn start(&mut self, action: &StartAction) -> Result<(), String> {
        self.receipt = None;
        self.outcome = None;
        self.intent_id = None;
        self.started = false;
        let arg = action
            .args
            .first()
            .and_then(ArgValue::name)
            .unwrap_or_default()
            .to_string();
        let active = match action.primitive.as_str() {
            "mine" => Active::Mine(Mine {
                name: arg,
                target: None,
                waited: 0,
                tool_waited: 0,
                hand: HandSelect::default(),
                pickup: None,
            }),
            "place" => Active::Place(Place {
                block: arg,
                target: None,
                sent: false,
                waited: 0,
                hand: HandSelect::default(),
                rejected: Vec::new(),
                clear_clicks: 0,
            }),
            "craft" => Active::Craft(Craft::new(arg)?),
            "eat" => Active::Eat(Eat {
                item: arg,
                sent: false,
                waited: 0,
                hand: HandSelect::default(),
            }),
            "explore" => Active::Explore(Explore::new(arg, &mut self.explore_memory)?),
            "attack" => Active::Attack(Attack {
                kind: arg,
                entity: None,
                target: None,
                waited: 0,
            }),
            "equip" => Active::Equip(Equip {
                item: arg,
                stage: EquipStage::Select,
                waited: 0,
            }),
            "wait" => {
                let ticks = action
                    .args
                    .first()
                    .and_then(ArgValue::as_int)
                    .unwrap_or(1)
                    .max(1) as u32;
                Active::Wait(Wait {
                    remaining: ticks,
                    total: ticks,
                })
            }
            "observe" => {
                let fact = match action.args.first() {
                    Some(ArgValue::Fact(call)) => call.clone(),
                    _ => return Err("observe needs a fact call".to_owned()),
                };
                Active::Observe(Observe {
                    fact,
                    target: None,
                    waited: 0,
                })
            }
            "move_to" => Active::Move(Move::new(arg)),
            other => return Err(format!("unknown primitive `{other}`")),
        };
        self.active = Some(active);
        Ok(())
    }

    /// The receipt settled. See [`Active::continues_after`] for which
    /// primitives stay active and which side effect each one needs first.
    pub fn settle(&mut self, outcome: Outcome) {
        self.receipt = Some(outcome.clone());
        if let Some(active) = &mut self.active {
            match active {
                Active::Craft(craft) => craft.request_close(outcome.clone()),
                Active::Mine(mine) if outcome == Outcome::Confirmed => mine.begin_pickup(),
                _ => {}
            }
        }
        let keep_going = self
            .active
            .as_ref()
            .is_some_and(|active| active.continues_after(&outcome));
        if !keep_going {
            self.finish(outcome);
        }
    }

    /// The driver registered the intent for the pending action.
    pub fn registered(&mut self, intent_id: u64) {
        self.intent_id = Some(intent_id);
        self.started = true;
    }

    pub fn intent_id(&self) -> Option<u64> {
        self.intent_id
    }

    pub fn outcome(&self) -> Option<&Outcome> {
        self.outcome.as_ref()
    }

    pub fn is_active(&self) -> bool {
        self.active.is_some() && self.outcome.is_none()
    }

    /// Whether the active primitive is a continuous one (`explore`,
    /// `move_to`, `wait`): the interpreter stops those by releasing them, not
    /// by waiting for an outcome.
    pub fn is_continuous(&self) -> bool {
        self.is_active()
            && matches!(
                self.active,
                Some(Active::Explore(_) | Active::Move(_) | Active::Wait(_))
            )
    }

    /// Cancel the active primitive as `Preempted` (the interpreter's flight
    /// deadline fired). Without this the primitive would keep running while
    /// the interpreter retries, and the retry would be dropped as "busy".
    pub fn cancel(&mut self) -> Option<Outcome> {
        if !self.is_active() {
            return None;
        }
        self.finish(Outcome::Preempted);
        // A later run must never inherit this run's receipt.
        self.intent_id = None;
        self.started = false;
        self.outcome.clone()
    }

    pub fn tick(&mut self, ctx: &mut Ctx<'_>) -> TickResult {
        if let Some(outcome) = &self.outcome {
            return TickResult {
                frame: idle(ctx),
                pending: None,
                outcome: Some(outcome.clone()),
                events: Vec::new(),
            };
        }
        // A failed or timed-out receipt ends the primitive. See
        // [`Active::continues_after`] for the primitives that keep going.
        if let Some(receipt) = self.receipt.clone() {
            let keep_going = receipt == Outcome::Confirmed
                && self
                    .active
                    .as_ref()
                    .is_some_and(|active| active.continues_after(&receipt));
            if !keep_going {
                self.finish(receipt.clone());
                return TickResult {
                    frame: idle(ctx),
                    pending: None,
                    outcome: Some(receipt),
                    events: Vec::new(),
                };
            }
        }
        let Some(active) = &mut self.active else {
            return TickResult {
                frame: idle(ctx),
                pending: None,
                outcome: self.outcome.clone(),
                events: Vec::new(),
            };
        };
        let (frame, pending, outcome) = match active {
            Active::Mine(state) => state.tick(ctx, self.started),
            Active::Place(state) => state.tick(ctx, self.started),
            Active::Craft(state) => state.tick(ctx, self.started),
            Active::Eat(state) => state.tick(ctx, self.started),
            Active::Explore(state) => state.tick(ctx, self.started, &mut self.explore_memory),
            Active::Attack(state) => state.tick(ctx, self.started),
            Active::Equip(state) => state.tick(ctx),
            Active::Wait(state) => state.tick(ctx),
            Active::Observe(state) => state.tick(ctx, self.started),
            Active::Move(state) => state.tick(ctx, self.started),
        };
        if let Some(outcome) = &outcome {
            self.finish(outcome.clone());
        }
        let events = match &mut self.active {
            Some(Active::Explore(state)) => std::mem::take(&mut state.events),
            _ => Vec::new(),
        };
        TickResult {
            frame,
            pending,
            outcome,
            events,
        }
    }

    fn finish(&mut self, outcome: Outcome) {
        self.outcome = Some(outcome);
        self.active = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use praxion_rules::registry::{PrimitiveKind, Primitives};

    #[test]
    fn is_continuous_matches_the_registry() {
        // `Runner::is_continuous` lists the continuous primitives by hand; a
        // new continuous record would otherwise never be released (review,
        // 2026-11-03).
        let continuous: Vec<&str> = Primitives::standard()
            .iter()
            .filter(|record| record.kind == PrimitiveKind::Continuous)
            .map(|record| record.name)
            .collect();
        assert_eq!(continuous, ["explore", "move_to", "wait"]);
        for (name, arg) in [("explore", "#logs"), ("move_to", "0, 64, 0"), ("wait", "5")] {
            let mut runner = Runner::new();
            runner
                .start(&StartAction {
                    primitive: name.into(),
                    args: vec![ArgValue::Name(arg.into())],
                })
                .unwrap_or_else(|error| panic!("{name}: {error}"));
            assert!(runner.is_continuous(), "{name}");
        }
    }
}
