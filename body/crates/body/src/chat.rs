//! The body's chat rules: who is obeyed, which words the body parses itself, and
//! how much it may say.
//!
//! Decisions: 2026-11-09 "Five next areas" (the bot speaks Turkish; `dur`,
//! `iptal`, `durum`, `devam` with English equivalents), 2026-11-09 "Chat:
//! control words bare, tasks with a prefix" (the four verbs always act; a task
//! needs a configurable prefix, `p:` by default; the operator's other chat stays
//! chat and never reaches the brain) and architecture §7 (only the operator is
//! obeyed, identified by the sender UUID; control verbs work with the brain
//! down). The chat spike (`docs/sessions/09-commands.md` "2026-11-09 chat
//! spike") measured the inputs: on the offline 26.3 server a player's line
//! arrives as `ClientboundPlayerChat` whose `sender` is the offline UUID
//! `MD5("OfflinePlayer:<name>")` (version 3), the bot receives its own line
//! back, and `/me` arrives as `ClientboundDisguisedChat` (never an input).
//!
//! Everything here is pure: the driver feeds it packets and ticks.

use std::collections::{BTreeSet, VecDeque};

/// The longest line the body sends, in characters. Chat's wire limit is 256;
/// the brain's templates stay under 200.
pub const MAX_LINE_CHARS: usize = 200;
/// Ticks between two lines the body sends (a flood guard; the brain owns the
/// real rate limits, critique finding 4).
pub const MIN_GAP_TICKS: u64 = 10;
/// Lines the body holds before it drops new ones.
pub const MAX_QUEUE: usize = 16;
/// A queued line older than this is dropped rather than said out of context
/// (30 s at 20 TPS).
pub const STALE_TICKS: u64 = 600;
/// The prefix an operator's task line needs by default: `p: 8 taş getir`
/// (decision 2026-11-09 "Chat: control words bare, tasks with a prefix"). The
/// body carries no other operator chat to the brain, so free talk stays talk.
pub const DEFAULT_TASK_PREFIX: &str = "p:";

/// The offline-mode UUID of a player name, as the server derives it:
/// `UUID.nameUUIDFromBytes("OfflinePlayer:" + name)` (MD5, version 3, RFC 4122
/// variant). Verified against the spike's measured value in the tests.
pub fn offline_uuid(name: &str) -> u128 {
    let mut digest = md5(format!("OfflinePlayer:{name}").as_bytes());
    digest[6] = (digest[6] & 0x0f) | 0x30;
    digest[8] = (digest[8] & 0x3f) | 0x80;
    u128::from_be_bytes(digest)
}

/// The identities the body obeys.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Operators {
    own: u128,
    allowed: BTreeSet<u128>,
}

impl Operators {
    /// `own` is the body's own login name (its lines come back to it and are
    /// never read); `operators` are the player names that may give orders.
    pub fn new(own: &str, operators: &[String]) -> Self {
        let own = offline_uuid(own);
        let allowed = operators
            .iter()
            .map(|name| offline_uuid(name.trim()))
            .filter(|uuid| *uuid != own)
            .collect();
        Self { own, allowed }
    }

    /// Whether `sender` may give orders. Never true for the body itself.
    pub fn obeys(&self, sender: u128) -> bool {
        sender != self.own && self.allowed.contains(&sender)
    }

    /// Whether no operator is configured (the body then obeys nobody).
    pub fn is_empty(&self) -> bool {
        self.allowed.is_empty()
    }
}

/// A word the body acts on itself, before any brain call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    /// `dur` / `stop`: cancel the running command and pause the brain's picking.
    Stop,
    /// `iptal` / `cancel`: cancel the running command; picking continues.
    Cancel,
    /// `durum` / `status`: the body answers from its own facts.
    Status,
    /// `devam` / `resume` / `carry on`: give the body back to what was stopped.
    Resume,
}

impl Control {
    /// The name the forwarded `chat_received` event carries.
    pub fn name(self) -> &'static str {
        match self {
            Control::Stop => "stop",
            Control::Cancel => "cancel",
            Control::Status => "status",
            Control::Resume => "resume",
        }
    }
}

/// The control verb in `text`, if the whole line is one. Case-insensitive
/// (Turkish included), surrounding space and `.`/`!`/`?`/`,` ignored. Turkish
/// first, English as the table the tests use.
pub fn parse_control(text: &str) -> Option<Control> {
    let lowered = lower_turkish(
        text.trim()
            .trim_matches(|c: char| matches!(c, '.' | '!' | '?' | ',') || c.is_whitespace()),
    );
    let word = lowered.split_whitespace().collect::<Vec<_>>().join(" ");
    match word.as_str() {
        "dur" | "stop" => Some(Control::Stop),
        "iptal" | "cancel" => Some(Control::Cancel),
        "durum" | "status" => Some(Control::Status),
        "devam" | "devam et" | "resume" | "carry on" => Some(Control::Resume),
        _ => None,
    }
}

/// Lowercase the Turkish way: `İ` becomes `i` (plain lowercase gives `i` plus a
/// combining dot, U+0307, which equals no word) and `I` becomes `ı`. ASCII words
/// are unaffected (`DUR` and `STOP` still match).
fn lower_turkish(text: &str) -> String {
    text.chars()
        .flat_map(|character| match character {
            'İ' => 'i'.to_lowercase(),
            'I' => 'ı'.to_lowercase(),
            other => other.to_lowercase(),
        })
        .collect()
}

/// The task text `text` carries: `text` without its prefix, or `None` when the
/// line is plain chat. The prefix compares ASCII case-insensitively (`P:` is
/// the same); an empty prefix means every operator line is a task.
pub fn task_text(text: &str, prefix: &str) -> Option<String> {
    let text = text.trim();
    if prefix.is_empty() {
        return Some(text.to_owned());
    }
    let head: String = text.chars().take(prefix.chars().count()).collect();
    if !head.eq_ignore_ascii_case(prefix) {
        return None;
    }
    let task = text[head.len()..].trim();
    if task.is_empty() {
        None
    } else {
        Some(task.to_owned())
    }
}


/// What the operator filter decided about one `ClientboundPlayerChat`.
#[derive(Debug, PartialEq, Eq)]
pub enum Inbound {
    /// Not from an operator (or the body's own echo), or the operator's chat
    /// without the task prefix: dropped, never answered, never forwarded.
    Ignored,
    /// A control verb the body acts on.
    Control(Control),
    /// A task line: the text without its prefix, forwarded to the brain.
    Task(String),
}

/// Filter one line by sender, then decide what it is. Free text from anyone
/// else is never an instruction (architecture §7); the operator's plain chat
/// without the task prefix is not one either (decision 2026-11-09 "Chat:
/// control words bare, tasks with a prefix").
pub fn classify(operators: &Operators, sender: u128, text: &str, prefix: &str) -> Inbound {
    if !operators.obeys(sender) {
        return Inbound::Ignored;
    }
    if let Some(control) = parse_control(text) {
        return Inbound::Control(control);
    }
    match task_text(text, prefix) {
        Some(task) => Inbound::Task(task),
        None => Inbound::Ignored,
    }
}

/// Why a line did not go out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropReason {
    TooLong,
    QueueFull,
    Stale,
}

impl DropReason {
    pub fn as_str(self) -> &'static str {
        match self {
            DropReason::TooLong => "too_long",
            DropReason::QueueFull => "queue_full",
            DropReason::Stale => "stale",
        }
    }
}

/// What [`Outbound::take`] returns: the `(id, text)` to send now, and the
/// `(id, reason)` of lines dropped on the way.
pub type Taken = (Option<(String, String)>, Vec<(String, DropReason)>);

/// The lines waiting to be said, one per [`MIN_GAP_TICKS`].
#[derive(Debug, Default)]
pub struct Outbound {
    queue: VecDeque<(String, String, u64)>,
    last_sent: Option<u64>,
}

impl Outbound {
    /// Queue a line. A line over [`MAX_LINE_CHARS`], control characters
    /// included, or past [`MAX_QUEUE`] is refused with its reason.
    pub fn push(&mut self, id: String, text: String, tick: u64) -> Result<(), DropReason> {
        if text.chars().count() > MAX_LINE_CHARS || text.chars().any(char::is_control) {
            return Err(DropReason::TooLong);
        }
        if self.queue.len() >= MAX_QUEUE {
            return Err(DropReason::QueueFull);
        }
        self.queue.push_back((id, text, tick));
        Ok(())
    }

    /// The line to send at `tick`, if the gap allows one, plus the stale lines
    /// dropped on the way (oldest first).
    pub fn take(&mut self, tick: u64) -> Taken {
        let mut dropped = Vec::new();
        while let Some((_, _, queued)) = self.queue.front() {
            if tick.saturating_sub(*queued) > STALE_TICKS {
                let (id, _, _) = self.queue.pop_front().expect("front exists");
                dropped.push((id, DropReason::Stale));
            } else {
                break;
            }
        }
        if self
            .last_sent
            .is_some_and(|last| tick.saturating_sub(last) < MIN_GAP_TICKS)
        {
            return (None, dropped);
        }
        match self.queue.pop_front() {
            Some((id, text, _)) => {
                self.last_sent = Some(tick);
                (Some((id, text)), dropped)
            }
            None => (None, dropped),
        }
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}

/// MD5 (RFC 1321). Only for the offline UUID: no other use, and no dependency
/// for 60 lines.
fn md5(input: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6,
        10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let k: Vec<u32> = (0..64)
        .map(|i| ((i as f64 + 1.0).sin().abs() * 4_294_967_296.0) as u32)
        .collect();
    let mut message = input.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&((input.len() as u64) * 8).to_le_bytes());

    let (mut a0, mut b0, mut c0, mut d0) =
        (0x6745_2301u32, 0xefcd_ab89u32, 0x98ba_dcfeu32, 0x1032_5476u32);
    for chunk in message.chunks(64) {
        let m: Vec<u32> = chunk
            .chunks(4)
            .map(|word| u32::from_le_bytes([word[0], word[1], word[2], word[3]]))
            .collect();
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let rotated = a
                .wrapping_add(f)
                .wrapping_add(k[i])
                .wrapping_add(m[g])
                .rotate_left(S[i]);
            (a, d, c) = (d, c, b);
            b = b.wrapping_add(rotated);
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    let mut out = [0u8; 16];
    for (slot, value) in out.chunks_mut(4).zip([a0, b0, c0, d0]) {
        slot.copy_from_slice(&value.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hyphenated(uuid: u128) -> String {
        let hex = format!("{uuid:032x}");
        format!(
            "{}-{}-{}-{}-{}",
            &hex[0..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..32]
        )
    }

    #[test]
    fn md5_matches_the_rfc_vectors() {
        let hex = |digest: [u8; 16]| digest.iter().map(|b| format!("{b:02x}")).collect::<String>();
        assert_eq!(hex(md5(b"")), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(hex(md5(b"abc")), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            hex(md5(b"12345678901234567890123456789012345678901234567890123456789012345678901234567890")),
            "57edf4a22be3c955ac49da2e2107b67a"
        );
    }

    #[test]
    fn the_offline_uuid_matches_what_the_spike_measured() {
        // docs/sessions/09-commands.md: A and B on the pinned 26.3 server.
        assert_eq!(hyphenated(offline_uuid("A")), "b76d4ad3-8d01-3150-b333-68cfb4257774");
        assert_eq!(hyphenated(offline_uuid("B")), "f44cd58d-7902-30e7-8bde-679d783ee631");
    }

    #[test]
    fn only_a_configured_operator_is_obeyed_and_never_the_body_itself() {
        let operators = Operators::new("Praxion", &["Fatih".to_owned(), "Praxion".to_owned()]);
        assert!(operators.obeys(offline_uuid("Fatih")));
        assert!(!operators.obeys(offline_uuid("Stranger")));
        assert!(!operators.obeys(offline_uuid("Praxion")), "the echo of its own line");
        assert!(Operators::new("Praxion", &[]).is_empty());
        assert!(!Operators::new("Praxion", &[]).obeys(offline_uuid("Fatih")));
    }

    #[test]
    fn control_verbs_are_turkish_with_english_equivalents() {
        for (text, control) in [
            ("dur", Control::Stop),
            ("DUR!", Control::Stop),
            ("dur?", Control::Stop),
            ("stop", Control::Stop),
            ("iptal", Control::Cancel),
            // Turkish dotted capital İ must lowercase to a plain i (review
            // finding 6: with plain lowercasing it would never match).
            ("İptal", Control::Cancel),
            ("İPTAL", Control::Cancel),
            ("cancel", Control::Cancel),
            ("durum", Control::Status),
            ("Durum.", Control::Status),
            ("status", Control::Status),
            ("devam", Control::Resume),
            ("devam et", Control::Resume),
            ("carry on", Control::Resume),
            ("  Resume ", Control::Resume),
        ] {
            assert_eq!(parse_control(text), Some(control), "{text:?}");
        }
        for text in ["8 taş getir", "dur biraz", "stop it now", "", "durumu söyle"] {
            assert_eq!(parse_control(text), None, "{text:?} is free text, not a verb");
        }
    }

    #[test]
    fn a_task_needs_the_prefix_and_the_prefix_is_not_forwarded() {
        assert_eq!(task_text("p: 8 taş getir", "p:"), Some("8 taş getir".into()));
        assert_eq!(task_text("  P:8 taş getir ", "p:"), Some("8 taş getir".into()));
        assert_eq!(task_text("8 taş getir", "p:"), None);
        assert_eq!(task_text("p:", "p:"), None, "a bare prefix is not a task");
        // Configured to no prefix: every operator line is a task.
        assert_eq!(task_text("8 taş getir", ""), Some("8 taş getir".into()));
    }

    #[test]
    fn a_stranger_saying_stop_changes_nothing() {
        let operators = Operators::new("Praxion", &["Fatih".to_owned()]);
        let stranger = offline_uuid("Mallory");
        assert_eq!(classify(&operators, stranger, "dur", "p:"), Inbound::Ignored);
        assert_eq!(classify(&operators, stranger, "p: 8 taş getir", "p:"), Inbound::Ignored);
        let fatih = offline_uuid("Fatih");
        assert_eq!(
            classify(&operators, fatih, "dur", "p:"),
            Inbound::Control(Control::Stop)
        );
        assert_eq!(
            classify(&operators, fatih, "p: 8 taş getir", "p:"),
            Inbound::Task("8 taş getir".into())
        );
        // The operator's plain chat stays chat: no prefix, no task.
        assert_eq!(classify(&operators, fatih, "8 taş getir", "p:"), Inbound::Ignored);
        assert_eq!(
            classify(&operators, fatih, "keşke biraz taş olsa", "p:"),
            Inbound::Ignored
        );
    }

    #[test]
    fn the_outbound_queue_caps_length_rate_and_age() {
        let mut out = Outbound::default();
        assert_eq!(out.push("a".into(), "x".repeat(201), 0), Err(DropReason::TooLong));
        assert_eq!(out.push("a".into(), "iki\nsatır".into(), 0), Err(DropReason::TooLong));
        assert!(out.push("a".into(), "x".repeat(200), 0).is_ok());
        assert!(out.push("b".into(), "ikinci".into(), 0).is_ok());
        // One per gap.
        assert_eq!(out.take(0).0.map(|(id, _)| id), Some("a".into()));
        assert_eq!(out.take(MIN_GAP_TICKS - 1).0, None);
        assert_eq!(out.take(MIN_GAP_TICKS).0.map(|(id, _)| id), Some("b".into()));
        // A flood past the queue bound is refused.
        for number in 0..MAX_QUEUE {
            assert!(out.push(format!("f{number}"), "x".into(), 100).is_ok());
        }
        assert_eq!(out.push("over".into(), "x".into(), 100), Err(DropReason::QueueFull));
        // Old lines are dropped as stale, newest kept.
        let (sent, dropped) = out.take(100 + STALE_TICKS + 1);
        assert!(sent.is_none());
        assert_eq!(dropped.len(), MAX_QUEUE);
        assert!(dropped.iter().all(|(_, reason)| *reason == DropReason::Stale));
        assert!(out.is_empty());
    }
}
