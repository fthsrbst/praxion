//! Typed events and the small data types they carry.

use serde::{Deserialize, Serialize};

/// The version pins recorded once at the start of a run.
///
/// The body fills these from `versions/manifest.toml`; the log crate stays
/// independent of the manifest reader.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Versions {
    /// The Praxion build, or `"dev"` for an unversioned build.
    pub praxion: String,
    /// The Minecraft version, for example `"26.3"`.
    pub minecraft: String,
    /// The protocol number the forwarder accepts.
    pub protocol: u32,
    /// The pinned Azalea commit.
    pub azalea: String,
    /// The pinned Rust toolchain.
    pub rust: String,
}

/// What causally attributes evidence to an intent (architecture §5.3).
///
/// A tick at or after the start is necessary but never sufficient: the causal
/// key must match too. `Nonce` is reserved for Demo-1's injected events and is
/// unused in Demo-0.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Attribution {
    /// No causal key: an action whose evidence is an effect, not a block change.
    None,
    /// The client packet's block-change sequence number.
    BlockSeq { seq: u32 },
    /// A menu action's window state id and slot.
    SlotClick { state_id: u32, slot: i16 },
    /// A per-run nonce on an injected event (Demo-1, unused in Demo-0).
    Nonce { nonce: u64 },
}

/// How an action settled (architecture §5.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// The evidence contract was met.
    Confirmed,
    /// The window closed without evidence, or evidence refuted it.
    Unconfirmed,
    /// The caller's own deadline expired before the receipt settled.
    Timeout,
    /// A reflex, take-over or new command abandoned the action.
    Preempted,
    /// The settlement is no longer retained by the engine's late-evidence
    /// window. This is an answer to a late lookup; the writer never logs it.
    Expired,
}

/// One piece of evidence that took part in a settlement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRef {
    /// The evidence kind, for example `"block_air"` or `"block_changed_ack"`.
    pub kind: String,
    /// The body tick on which the observation arrived.
    pub tick: u64,
}

/// One line of the log, before the envelope fields are added.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventKind {
    /// First event of a run: the pins it was played with.
    RunStarted { versions: Versions },
    /// The actuator started a primitive and registered its evidence contract.
    IntentStarted {
        intent_id: u64,
        primitive: String,
        args: Vec<String>,
        start_tick: u64,
        attribution: Attribution,
    },
    /// A receipt reached its outcome and will not change again.
    ReceiptSettled {
        receipt_id: u64,
        intent_id: u64,
        outcome: Outcome,
        evidence: Vec<EvidenceRef>,
        #[serde(default)]
        note: Option<String>,
    },
    /// Evidence arrived after its receipt had already settled.
    ///
    /// The payload field is `evidence_kind`, not `kind`: `kind` is already the
    /// event tag of every line, and JSON cannot hold both without a duplicate
    /// key.
    LateEvidence {
        receipt_id: u64,
        evidence_kind: String,
    },
    /// The followed connection lost framing or decoding.
    WorldDesync { reason: String },
    /// A bounded, aggregated entry from perception's ledger of denied attempts
    /// and privileged reads.
    Ledger {
        query: String,
        fact: String,
        count: u64,
        first_tick: u64,
        last_tick: u64,
    },
    /// The brain (or the body on its behalf) pushed an intent onto the stack.
    ///
    /// `tier` is one of `survival`, `command`, `maintenance`, `projects`.
    IntentPushed {
        intent_id: u64,
        tier: String,
        skill: String,
        args: Vec<String>,
    },
    /// An intent's state changed: `running`, `suspended`,
    /// `frozen_for_carry_on` or `done`. The body alone writes `done`.
    IntentState { intent_id: u64, state: String },
    /// The owner of the body changed; `generation` is the body's counter after
    /// the change. `owner` is `idle`, `skill:<id>`, `reflex:<kind>` or
    /// `maintenance:<need>`.
    OwnerChanged { generation: u32, owner: String },
    /// A preempted or suspended intent must be replanned before it resumes
    /// (architecture §5.4).
    ReplanNeeded { intent_id: u64 },
    /// A primitive ended and will not tick again. `outcome` is `confirmed`,
    /// `timeout`, `preempted` or `failed`; `failure_kind` carries the typed
    /// reason of a failure. `ticks` is how long the primitive ran.
    PrimitiveFinished {
        primitive: String,
        args: Vec<String>,
        outcome: String,
        #[serde(default)]
        failure_kind: Option<String>,
        ticks: u64,
    },
    /// A skill frame was pushed, completed or failed. `depth` is one-based
    /// (the root skill is depth 1) and `outcome` is `pushed`, `done` or
    /// `failed`.
    SkillFrame {
        skill: String,
        depth: u32,
        outcome: String,
        #[serde(default)]
        failure_kind: Option<String>,
    },
    /// The highest true rule of a skill changed. Emitted only on a change, so
    /// a steady skill costs one line, not one per tick.
    RuleSelected { skill: String, rule: u32 },
    /// The frontier `explore` chose to walk to, with its score (the squared
    /// distance used by the ordering) and how many frontier cells were
    /// candidates.
    ExploreFrontier {
        cell: String,
        score: i64,
        candidates: u32,
    },
    /// The brain's command lifecycle (proposal §2.2). A receipt is an event,
    /// not a fourth channel: the harness, a replay and the live brain all see
    /// the same line.
    ///
    /// `state` is `pending`, `accepted`, `rejected`, `expired` or `settled`.
    /// `pending` is written when the driver drains its inbox; `accepted` after
    /// `Arbiter::command` returned, with the generation it bumped to. A
    /// `rejected` carries `reason` (`stale_generation`, `unknown_skill`,
    /// `stack_full`, `bad_condition`, `bad_args`); a `settled` carries
    /// `outcome` (`done`, `failed`, `cancelled` or `popped`), the typed
    /// `failure_kind`, and the guard's `condition` result (`true`, `false` or
    /// `unknown`). `cancelled` is a brain `pop` of the running intent: the stop
    /// is confirmed by this receipt (architecture §5.4).
    CommandReceipt {
        command_id: String,
        state: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        intent_id: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        generation: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outcome: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        failure_kind: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        condition: Option<String>,
    },
    /// A line the operator typed in game chat that is not a body-parsed control
    /// verb, forwarded for the brain to map. `control` is set when the body
    /// parsed a control verb (`stop`, `cancel`, `status`, `resume`) and already
    /// acted on it; the brain only keeps its own picking in step.
    ChatReceived {
        sender: String,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        control: Option<String>,
    },
    /// A `say` line went out as a `ServerboundChat`. `id` is the brain's line
    /// id (`b-...` for a line the body composed itself).
    ChatSent { id: String, text: String },
    /// A `say` line was not sent: `too_long`, `stale`, `queue_full` or
    /// `send_failed`.
    ChatDropped { id: String, reason: String },
    /// The player died (decision 2026-11-10 "Death is an event"). The envelope's
    /// `tick` is the tick the body handled the death on. `message` is the
    /// server's death message (`PlayerCombatKill`, for example "Praxion was
    /// blown up by Creeper") when the body saw it, else `null`. The field is not
    /// called `tick` or `kind`: both are envelope keys.
    Died {
        #[serde(default)]
        message: Option<String>,
    },
    /// The brain should wake. Demo-0 has no brain yet, so this is only logged.
    WakeBrain { cause: String, detail: String },
    /// Free-form operator or test note.
    Note { text: String },
}

/// One complete log line: the envelope plus its typed payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// Strictly increasing from 0, one per event.
    pub seq: u64,
    /// The body tick the event happened in.
    pub tick: u64,
    /// The arrival timestamp in nanoseconds, as the caller observed it.
    pub ts_ns: u64,
    #[serde(flatten)]
    pub event: EventKind,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(kind: EventKind) -> EventKind {
        let event = Event {
            seq: 0,
            tick: 12,
            ts_ns: 1,
            event: kind,
        };
        let line = serde_json::to_string(&event).expect("serialises");
        let back: Event = serde_json::from_str(&line).expect("deserialises");
        back.event
    }

    #[test]
    fn the_arbiter_events_round_trip_with_their_tags() {
        let cases = [
            EventKind::IntentPushed {
                intent_id: 3,
                tier: "command".to_owned(),
                skill: "get_wood".to_owned(),
                args: vec!["oak".to_owned()],
            },
            EventKind::IntentState {
                intent_id: 3,
                state: "suspended".to_owned(),
            },
            EventKind::OwnerChanged {
                generation: 9,
                owner: "reflex:creeper".to_owned(),
            },
            EventKind::ReplanNeeded { intent_id: 3 },
            EventKind::PrimitiveFinished {
                primitive: "mine".to_owned(),
                args: vec!["oak_log".to_owned()],
                outcome: "failed".to_owned(),
                failure_kind: Some("no_block".to_owned()),
                ticks: 41,
            },
            EventKind::SkillFrame {
                skill: "get_wood".to_owned(),
                depth: 2,
                outcome: "failed".to_owned(),
                failure_kind: Some("no_block".to_owned()),
            },
            EventKind::RuleSelected {
                skill: "get_wood".to_owned(),
                rule: 2,
            },
            EventKind::ExploreFrontier {
                cell: "4, 64, -9".to_owned(),
                score: 97,
                candidates: 12,
            },
            EventKind::WakeBrain {
                cause: "reflex".to_owned(),
                detail: "creeper ran 40 ticks".to_owned(),
            },
            EventKind::CommandReceipt {
                command_id: "c-0007".to_owned(),
                state: "settled".to_owned(),
                intent_id: Some(3),
                generation: Some(9),
                outcome: Some("done".to_owned()),
                failure_kind: None,
                reason: None,
                condition: Some("true".to_owned()),
            },
        ];
        for case in cases {
            let back = round_trip(case.clone());
            assert_eq!(back, case);
        }

        let line = serde_json::to_string(&Event {
            seq: 1,
            tick: 2,
            ts_ns: 3,
            event: EventKind::OwnerChanged {
                generation: 4,
                owner: "reflex:lava".to_owned(),
            },
        })
        .unwrap();
        assert!(line.contains("\"kind\":\"owner_changed\""), "{line}");
        assert!(line.contains("\"generation\":4"), "{line}");
    }

    #[test]
    fn a_death_round_trips_with_or_without_its_message() {
        for message in [Some("Praxion was blown up by Creeper".to_owned()), None] {
            let case = EventKind::Died { message };
            assert_eq!(round_trip(case.clone()), case);
        }
        let line = serde_json::to_string(&Event {
            seq: 3,
            tick: 4660,
            ts_ns: 5,
            event: EventKind::Died { message: None },
        })
        .unwrap();
        assert_eq!(
            line,
            r#"{"seq":3,"tick":4660,"ts_ns":5,"kind":"died","message":null}"#
        );
        let back: Event =
            serde_json::from_str(r#"{"seq":3,"tick":4660,"ts_ns":5,"kind":"died"}"#).unwrap();
        assert_eq!(back.event, EventKind::Died { message: None });
    }

    #[test]
    fn a_command_receipt_carries_only_the_fields_it_has() {
        let line = serde_json::to_string(&Event {
            seq: 7,
            tick: 8,
            ts_ns: 9,
            event: EventKind::CommandReceipt {
                command_id: "c-0001".to_owned(),
                state: "accepted".to_owned(),
                intent_id: Some(1),
                generation: Some(4),
                outcome: None,
                failure_kind: None,
                reason: None,
                condition: None,
            },
        })
        .unwrap();
        assert!(line.contains("\"kind\":\"command_receipt\""), "{line}");
        assert!(!line.contains("outcome"), "{line}");
        assert!(!line.contains("reason"), "{line}");
        assert!(!line.contains("condition"), "{line}");
    }
}
