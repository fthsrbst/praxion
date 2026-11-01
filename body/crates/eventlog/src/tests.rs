use std::fs::{OpenOptions, read_to_string};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::{
    Attribution, Event, EventKind, EvidenceRef, Outcome, Reader, ReadError, Replay, Versions,
    Writer, read, replay,
};
fn test_dir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/eventlog-tests")
        .join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn versions() -> Versions {
    Versions {
        praxion: "dev".to_owned(),
        minecraft: "26.3".to_owned(),
        protocol: 777,
        azalea: "8041a14".to_owned(),
        rust: "nightly-2026-08-20".to_owned(),
    }
}

#[test]
fn round_trip_every_kind() {
    let dir = test_dir("round-trip");
    let mut writer = Writer::create(&dir).unwrap();
    let t = 1_793_443_244_558_717_800;

    let expected = vec![
        (10, EventKind::RunStarted { versions: versions() }),
        (
            10,
            EventKind::IntentStarted {
                intent_id: 7,
                primitive: "mine".to_owned(),
                args: vec!["stone".to_owned()],
                start_tick: 9,
                attribution: Attribution::BlockSeq { seq: 42 },
            },
        ),
        (
            11,
            EventKind::ReceiptSettled {
                receipt_id: 3,
                intent_id: 7,
                outcome: Outcome::Confirmed,
                evidence: vec![
                    EvidenceRef { kind: "block_air".to_owned(), tick: 10 },
                    EvidenceRef { kind: "block_changed_ack".to_owned(), tick: 11 },
                ],
                note: Some("unrefuted".to_owned()),
            },
        ),
        (
            11,
            EventKind::LateEvidence {
                receipt_id: 3,
                evidence_kind: "block_air".to_owned(),
            },
        ),
        (
            12,
            EventKind::WorldDesync { reason: "tee gap".to_owned() },
        ),
        (
            12,
            EventKind::Ledger {
                query: "block".to_owned(),
                fact: "block_state".to_owned(),
                count: 2,
                first_tick: 4,
                last_tick: 12,
            },
        ),
        (13, EventKind::Note { text: "hello \"world\"\n".to_owned() }),
    ];

    for (tick, kind) in &expected {
        writer.append(*tick, t, kind.clone()).unwrap();
    }
    writer.flush().unwrap();
    drop(writer);

    let replay = read(dir.join(crate::FILE_NAME)).unwrap();
    assert!(replay.is_complete(), "{:?}", replay.error);
    let events = replay.events;
    assert_eq!(events.len(), expected.len());
    for (index, (event, (tick, kind))) in events.iter().zip(&expected).enumerate() {
        assert_eq!(event.seq, index as u64);
        assert_eq!(event.tick, *tick);
        assert_eq!(event.ts_ns, t);
        assert_eq!(&event.event, kind);
    }
}

#[test]
fn sequence_gap_is_refused() {
    let dir = test_dir("seq-gap");
    let good = Event {
        seq: 0,
        tick: 1,
        ts_ns: 2,
        event: EventKind::Note { text: "first".to_owned() },
    };
    let skipped = Event {
        seq: 2,
        tick: 1,
        ts_ns: 3,
        event: EventKind::Note { text: "third".to_owned() },
    };
    let path = dir.join(crate::FILE_NAME);
    let mut file = OpenOptions::new().create_new(true).write(true).open(&path).unwrap();
    writeln!(file, "{}", serde_json::to_string(&good).unwrap()).unwrap();
    writeln!(file, "{}", serde_json::to_string(&skipped).unwrap()).unwrap();
    drop(file);

    let replay = read(&path).unwrap();
    assert_eq!(replay.events.len(), 1);
    match replay.error {
        Some(crate::ReadError::SeqGap { line, expected, found }) => {
            assert_eq!((line, expected, found), (2, 1, 2));
        }
        other => panic!("expected a sequence gap, got {other:?}"),
    }
}

#[test]
fn truncated_last_line_is_reported_and_the_prefix_replays() {
    let dir = test_dir("truncated");
    let path = dir.join(crate::FILE_NAME);
    let mut writer = Writer::create(&dir).unwrap();
    writer.note(0, 1, "one").unwrap();
    writer.note(0, 2, "two").unwrap();
    writer.flush().unwrap();
    drop(writer);

    // A crash in the middle of the third line: valid JSON never finished.
    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(b"{\"seq\":2,\"tick\":0,\"ts_ns\":3,\"kind\":\"note\",\"text\":\"thr")
        .unwrap();
    drop(file);

    let first = read(&path).unwrap();
    assert_eq!(first.events.len(), 2);
    match first.error {
        Some(crate::ReadError::Truncated { line, .. }) => assert_eq!(line, 3),
        other => panic!("expected a truncated last line, got {other:?}"),
    }

    // The same prefix replays through an explicit reader.
    let text = read_to_string(&path).unwrap();
    let prefix = replay(Reader::new(text.as_bytes()));
    assert_eq!(prefix.events.len(), 2);
    assert!(!prefix.is_complete());
}

#[test]
fn a_small_log_round_trips() {
    let dir = test_dir("small");
    let mut writer = Writer::create(&dir).unwrap();
    writer.note(0, 1, "a").unwrap();
    writer.flush().unwrap();
    drop(writer);
    let replay: Replay = read(dir.join(crate::FILE_NAME)).unwrap();
    assert!(replay.is_complete());
    assert_eq!(replay.events.len(), 1);
}

#[test]
fn a_tick_change_and_drop_both_flush() {
    let dir = test_dir("flush");
    {
        let mut writer = Writer::create(&dir).unwrap();
        writer.note(0, 1, "tick zero").unwrap();
        // The tick change writes tick zero even without an explicit flush.
        writer.note(1, 2, "tick one").unwrap();
        // Dropping writes the tick in progress.
    }
    let replay = read(dir.join(crate::FILE_NAME)).unwrap();
    assert!(replay.is_complete(), "{:?}", replay.error);
    assert_eq!(replay.events.len(), 2);
    assert_eq!(replay.events[0].tick, 0);
    assert_eq!(replay.events[1].tick, 1);
}

#[test]
fn one_hundred_thousand_events_write_and_replay_under_a_second() {
    // The shared build machine also runs live game tests. A single wall-clock
    // sample can be disturbed by that load, so take the best of a few runs; a
    // genuinely slow implementation still fails every one of them.
    let mut best = std::time::Duration::MAX;
    let mut samples = Vec::new();
    for attempt in 0..10 {
        let dir = test_dir(&format!("perf-{attempt}"));
        let path = dir.join(crate::FILE_NAME);
        let started = Instant::now();

        let mut writer = Writer::create(&dir).unwrap();
        for index in 0..100_000u64 {
            // One tick: the writer holds the whole run in its buffer and flushes
            // once, so this measures the log path rather than 100 file flushes.
            writer
                .append(0, index, EventKind::Note { text: String::new() })
                .unwrap();
        }
        writer.flush().unwrap();
        drop(writer);

        let replay = read(&path).unwrap();
        let elapsed = started.elapsed();
        samples.push(elapsed);
        best = best.min(elapsed);
        assert!(replay.is_complete(), "{:?}", replay.error);
        assert_eq!(replay.events.len(), 100_000);
        assert_eq!(replay.events.last().unwrap().seq, 99_999);
        if elapsed < std::time::Duration::from_secs(1) {
            break;
        }
    }
    eprintln!("eventlog perf: 100k events written and replayed, samples {samples:?}");
    assert!(
        best < std::time::Duration::from_secs(1),
        "best of {samples:?} was still over one second"
    );
}

#[test]
fn reader_reports_expected_overflow() {
    let event = Event {
        seq: u64::MAX,
        tick: 0,
        ts_ns: 0,
        event: EventKind::Note {
            text: "max".to_owned(),
        },
    };
    let text = format!("{}\n", serde_json::to_string(&event).unwrap());
    let mut reader = Reader::new(text.as_bytes());
    // A file that reaches this expected value has no next valid sequence
    // number. The reader must report that instead of saturating and accepting
    // another u64::MAX line.
    reader.skip_to(u64::MAX);
    match reader.next() {
        Some(Err(ReadError::SeqOverflow { line })) => assert_eq!(line, 1),
        other => panic!("expected a sequence overflow, got {other:?}"),
    }
    assert!(reader.next().is_none());
}
