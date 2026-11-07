//! The query result type: `Known` with provenance and age, or `Unknown` with a
//! reason. "Unknown" is a value, never a missing answer (session 3).

use std::fmt;

/// Where a fact came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Provenance {
    /// A sight ray reached it (or an F3 fact, which the policy allows without
    /// a ray).
    Sight,
    /// A sound packet within the hearing radius.
    Hearing,
    /// An earlier sighting, kept with its age.
    Memory,
    /// Derived from other facts. Not produced in this step.
    Inference,
    /// Read from data the policy denies to the player. Every such read is
    /// ledgered; not produced in this step.
    Privileged,
}

impl Provenance {
    pub fn as_str(self) -> &'static str {
        match self {
            Provenance::Sight => "sight",
            Provenance::Hearing => "hearing",
            Provenance::Memory => "memory",
            Provenance::Inference => "inference",
            Provenance::Privileged => "privileged",
        }
    }
}

impl fmt::Display for Provenance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a query answered `Unknown`. One of exactly these five (architecture
/// §5.1).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Reason {
    /// The raw store has the fact, but no sight ray reaches it.
    OutOfSight,
    /// Beyond the sight range of the config.
    OutOfRange,
    /// The per-tick ray budget (or the entity cap) was spent.
    Budget,
    /// The policy does not allow this fact to this caller.
    Denied,
    /// No sighting or sound ever produced this fact.
    NeverSeen,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::OutOfSight => "out_of_sight",
            Reason::OutOfRange => "out_of_range",
            Reason::Budget => "budget",
            Reason::Denied => "denied",
            Reason::NeverSeen => "never_seen",
        }
    }
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Who is asking. `Reflex` may spend the reserved share of the ray budget.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Caller {
    Reflex,
    Other,
}

/// The answer to every perception query.
#[derive(Clone, Debug, PartialEq)]
pub enum Answer<T> {
    Known { value: T, provenance: Provenance, age_ticks: u64 },
    Unknown(Reason),
}

impl<T> Answer<T> {
    pub fn known(value: T, provenance: Provenance, age_ticks: u64) -> Self {
        Answer::Known { value, provenance, age_ticks }
    }

    pub fn is_known(&self) -> bool {
        matches!(self, Answer::Known { .. })
    }

    pub fn value(&self) -> Option<&T> {
        match self {
            Answer::Known { value, .. } => Some(value),
            Answer::Unknown(_) => None,
        }
    }

    pub fn provenance(&self) -> Option<Provenance> {
        match self {
            Answer::Known { provenance, .. } => Some(*provenance),
            Answer::Unknown(_) => None,
        }
    }

    pub fn age_ticks(&self) -> Option<u64> {
        match self {
            Answer::Known { age_ticks, .. } => Some(*age_ticks),
            Answer::Unknown(_) => None,
        }
    }

    pub fn reason(&self) -> Option<Reason> {
        match self {
            Answer::Known { .. } => None,
            Answer::Unknown(reason) => Some(*reason),
        }
    }
}
