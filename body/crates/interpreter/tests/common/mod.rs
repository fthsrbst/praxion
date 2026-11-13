//! Fixtures shared by the interpreter's unit and integration tests.
//!
//! Only the crate's public API is used, so this file compiles both as the
//! integration-test module `tests/common/mod.rs` and through the `#[path]`
//! include in `src/tests.rs`. The independent suite keeps its own assertions;
//! only the mock world and its helpers live here.

#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use praxion_rules::{ArgValue, CORE_COMPOSITION_DEPTH, FactCall, Program, load_files_with_depth};

use super::{Actuator, Decision, Facts, IntValue, Outcome, StartAction, Truth};

#[derive(Clone, Debug)]
pub struct Mock {
    pub bools: BTreeMap<String, bool>,
    pub ints: BTreeMap<String, i64>,
    pub unknown: BTreeSet<String>,
    pub duration: u64,
    pub outcome: Option<Outcome>,
    /// Per-primitive terminal outcomes, ahead of `outcome`, so a test can make
    /// two rules fail with different kinds.
    pub outcomes: BTreeMap<String, Outcome>,
    /// The terminal outcome a continuous action reports, for the tests of the
    /// explore/continuous path.
    pub continuous_outcome: Option<Outcome>,
    pub flight: Option<(u64, Outcome)>,
    pub starts: Vec<StartAction>,
    pub applies: Vec<StartAction>,
    pub cancels: u32,
    pub releases: u32,
}

impl Default for Mock {
    fn default() -> Self {
        Self {
            bools: BTreeMap::new(),
            ints: BTreeMap::new(),
            unknown: BTreeSet::new(),
            duration: 1,
            outcome: None,
            outcomes: BTreeMap::new(),
            continuous_outcome: None,
            flight: None,
            starts: Vec::new(),
            applies: Vec::new(),
            cancels: 0,
            releases: 0,
        }
    }
}

impl Facts for Mock {
    fn bool_fact(&mut self, call: &FactCall) -> Truth {
        if self.unknown.contains(&call.name) {
            return Truth::Unknown(call.clone());
        }
        let value = match call.name.as_str() {
            "has" | "near" => call
                .args
                .first()
                .and_then(ArgValue::name)
                .and_then(|name| self.bools.get(name))
                .copied()
                .unwrap_or(false),
            // The one fact with no argument that a test steers by name.
            "sky_above" => self.bools.get("sky_above").copied().unwrap_or(false),
            _ => false,
        };
        if value { Truth::True } else { Truth::False }
    }

    fn int_fact(&mut self, call: &FactCall) -> IntValue {
        if self.unknown.contains(&call.name) {
            return IntValue::Unknown(call.clone());
        }
        match call.name.as_str() {
            "count" => IntValue::Known(
                call.args
                    .first()
                    .and_then(ArgValue::name)
                    .and_then(|name| self.ints.get(name))
                    .copied()
                    .unwrap_or(0),
            ),
            _ => IntValue::Unknown(call.clone()),
        }
    }
}

impl Actuator for Mock {
    fn start(&mut self, action: &StartAction, tick: u64) -> u32 {
        self.starts.push(action.clone());
        let outcome = self
            .outcomes
            .get(&action.primitive)
            .cloned()
            .or_else(|| self.outcome.clone())
            .unwrap_or(Outcome::Confirmed);
        self.flight = Some((tick + self.duration, outcome));
        self.duration as u32
    }

    fn poll(&mut self, tick: u64) -> Option<Outcome> {
        match &self.flight {
            Some((settle_at, _)) if tick >= *settle_at => {
                let (_, outcome) = self.flight.take().expect("checked above");
                Some(outcome)
            }
            _ => None,
        }
    }

    fn cancel(&mut self) -> Option<Outcome> {
        if self.flight.take().is_some() {
            self.cancels += 1;
            Some(Outcome::Preempted)
        } else {
            None
        }
    }

    fn apply(&mut self, action: &StartAction, tick: u64) {
        self.applies.push(action.clone());
        if let Some(outcome) = self.continuous_outcome.clone() {
            self.flight = Some((tick + self.duration, outcome));
        }
    }

    fn release(&mut self) {
        self.releases += 1;
    }
}

pub fn program(source: &str) -> Program {
    praxion_rules::load(source).unwrap_or_else(|errors| {
        panic!(
            "program did not load: {:?}",
            errors.iter().map(|e| e.to_string()).collect::<Vec<_>>()
        )
    })
}

pub fn wait_ticks(decision: &Decision) -> i64 {
    match decision {
        Decision::Run { action, .. } if action.primitive == "wait" => {
            action.args[0].as_int().expect("wait takes an int")
        }
        other => panic!("expected a wait run, got {other:?}"),
    }
}

/// The core skills as shipped in `body/skills`.
pub fn core_program() -> Program {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../skills");
    let files: Vec<(String, String)> = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e == "skill"))
        .map(|path| {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            (name, fs::read_to_string(&path).unwrap())
        })
        .collect();
    load_files_with_depth(&files, CORE_COMPOSITION_DEPTH).unwrap_or_else(|errors| {
        panic!(
            "{:?}",
            errors.iter().map(ToString::to_string).collect::<Vec<_>>()
        )
    })
}
