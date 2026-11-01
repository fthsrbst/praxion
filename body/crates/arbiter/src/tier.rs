//! Tiers, needs and the intent stack (architecture §5.4, session 7).
//!
//! The stack is the body's record of what the brain asked for. The body alone
//! writes `done`; a new command suspends the old one; a preemption keeps the
//! entry and settles its in-flight action `preempted`.

use praxion_rules::ArgValue;

/// Owner tiers, highest priority first. The derived order is the priority
/// order, so `Tier::Survival < Tier::Command < Tier::Maintenance <
/// Tier::Projects`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tier {
    Survival,
    Command,
    Maintenance,
    Projects,
}

impl Tier {
    pub const fn name(self) -> &'static str {
        match self {
            Tier::Survival => "survival",
            Tier::Command => "command",
            Tier::Maintenance => "maintenance",
            Tier::Projects => "projects",
        }
    }
}

/// The four states of §5.4. The body alone writes [`IntentState::Done`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntentState {
    Running,
    Suspended,
    FrozenForCarryOn,
    Done,
}

impl IntentState {
    pub const fn name(self) -> &'static str {
        match self {
            IntentState::Running => "running",
            IntentState::Suspended => "suspended",
            IntentState::FrozenForCarryOn => "frozen_for_carry_on",
            IntentState::Done => "done",
        }
    }
}

/// One entry of the intent stack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Intent {
    pub intent_id: u64,
    pub tier: Tier,
    pub skill_ref: String,
    pub args: Vec<String>,
    pub state: IntentState,
}

/// What a push did to the stack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pushed {
    pub intent_id: u64,
    /// The entry the push suspended, if any.
    pub suspended: Option<u64>,
}

/// The stack refused a push.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StackFull;

/// The body's intent stack. Depth is capped by a number (measurement later);
/// eight is enough for Demo-0 and keeps the bound honest.
#[derive(Clone, Debug)]
pub struct IntentStack {
    entries: Vec<Intent>,
    next_id: u64,
    capacity: usize,
}

impl Default for IntentStack {
    fn default() -> Self {
        Self::new()
    }
}

impl IntentStack {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            next_id: 1,
            capacity: 8,
        }
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            ..Self::new()
        }
    }

    pub fn entries(&self) -> &[Intent] {
        &self.entries
    }

    pub fn depth(&self) -> usize {
        self.entries.len()
    }

    pub fn get(&self, intent_id: u64) -> Option<&Intent> {
        self.entries.iter().find(|entry| entry.intent_id == intent_id)
    }

    /// The entry that owns the command tier now.
    pub fn running(&self) -> Option<&Intent> {
        self.entries
            .iter()
            .find(|entry| entry.state == IntentState::Running)
    }

    /// Push a new intent. Every running entry is suspended first, so a new
    /// command suspends the old one instead of stacking two owners.
    pub fn push(
        &mut self,
        tier: Tier,
        skill_ref: impl Into<String>,
        args: Vec<String>,
    ) -> Result<Pushed, StackFull> {
        if self.entries.len() >= self.capacity {
            return Err(StackFull);
        }
        let suspended = self.suspend_running();
        let intent_id = self.next_id;
        self.next_id += 1;
        self.entries.push(Intent {
            intent_id,
            tier,
            skill_ref: skill_ref.into(),
            args,
            state: IntentState::Running,
        });
        Ok(Pushed {
            intent_id,
            suspended,
        })
    }

    /// Undo a [`IntentStack::push`]: drop the entry it added and put the entry
    /// it suspended back to `running`, so a push that cannot be honoured leaves
    /// the stack exactly as it was.
    pub fn rollback(&mut self, pushed: Pushed) -> bool {
        let Some(index) = self
            .entries
            .iter()
            .position(|entry| entry.intent_id == pushed.intent_id)
        else {
            return false;
        };
        self.entries.remove(index);
        if let Some(suspended) = pushed.suspended {
            self.set_state(suspended, IntentState::Running);
        }
        true
    }

    /// Suspend the running entry, if any. Returns its id.
    pub fn suspend_running(&mut self) -> Option<u64> {
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| entry.state == IntentState::Running)?;
        entry.state = IntentState::Suspended;
        Some(entry.intent_id)
    }

    /// Set a state. `Done` is the body's word; callers outside the body must
    /// not use it, so the arbiter is the only writer in this crate.
    pub fn set_state(&mut self, intent_id: u64, state: IntentState) -> bool {
        match self.get_mut(intent_id) {
            Some(entry) => {
                entry.state = state;
                true
            }
            None => false,
        }
    }

    fn get_mut(&mut self, intent_id: u64) -> Option<&mut Intent> {
        self.entries
            .iter_mut()
            .find(|entry| entry.intent_id == intent_id)
    }

    /// The body marks an intent done and drops older done entries.
    pub fn done(&mut self, intent_id: u64) -> bool {
        if !self.set_state(intent_id, IntentState::Done) {
            return false;
        }
        self.entries
            .retain(|entry| entry.state != IntentState::Done || entry.intent_id == intent_id);
        true
    }

    /// Pop one done entry. The brain may pop only `done` or `suspended`
    /// entries (§5.4); `done` is all Demo-0 needs.
    pub fn pop_done(&mut self) -> Option<Intent> {
        let index = self
            .entries
            .iter()
            .position(|entry| entry.state == IntentState::Done)?;
        Some(self.entries.remove(index))
    }

    /// The newest entry at a tier, for the owner's resume decision.
    pub fn newest_at(&self, tier: Tier) -> Option<&Intent> {
        self.entries
            .iter()
            .rev()
            .find(|entry| entry.tier == tier && entry.state != IntentState::Done)
    }
}

/// A need the arbiter balances. Hunger is the only one Demo-0 exercises; sleep
/// and shelter arrive with the skills that serve them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NeedKind {
    Eat,
    Sleep,
    Shelter,
}

impl NeedKind {
    pub const fn name(self) -> &'static str {
        match self {
            NeedKind::Eat => "eat",
            NeedKind::Sleep => "sleep",
            NeedKind::Shelter => "shelter",
        }
    }
}

/// The typed needs of one tick. Health and food are F3 facts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Needs {
    pub health: f32,
    pub food: u32,
}

impl Needs {
    /// Read the needs from a reflex view.
    pub fn from_view(view: &praxion_reflexes::View) -> Self {
        Self {
            health: view.health.unwrap_or(20.0),
            food: view.food.unwrap_or(20),
        }
    }
}

/// Convert a command argument to the string the intent entry stores.
pub fn arg_to_string(arg: &ArgValue) -> String {
    match arg {
        ArgValue::Int(value) => value.to_string(),
        ArgValue::Item(value)
        | ArgValue::Block(value)
        | ArgValue::EntityKind(value)
        | ArgValue::Name(value) => value.clone(),
        ArgValue::Bool(value) => value.to_string(),
        ArgValue::Fact(call) => call.name.clone(),
    }
}
