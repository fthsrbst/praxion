//! Corpus tests: every capture in `spikes/captures/` decodes, and the block
//! truth files in `spikes/round0/` match the final world of some capture.
//!
//! The captures are machine-local and ignored by git, so a machine without them
//! skips with an explicit message instead of failing.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use praxion_decoder::{Decoder, Dir, Event, Packet, State};
use praxion_tee::Reader;
use praxion_world::{BlockPos, World};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn captures() -> Vec<PathBuf> {
    let dir = repo_root().join("spikes/captures");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        eprintln!("SKIP corpus: {} does not exist on this machine", dir.display());
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "prxcap"))
        .collect();
    files.sort();
    if files.is_empty() {
        eprintln!("SKIP corpus: no *.prxcap files in {}", dir.display());
    }
    files
}

#[derive(Default)]
struct CaptureResult {
    packets: u64,
    desyncs: Vec<String>,
    failures: u64,
    by_group: BTreeMap<(State, Dir), u64>,
    frames: u64,
    bytes: u64,
    world: World,
    followed: Option<u32>,
}

fn apply(world: &mut World, followed: &mut Option<u32>, event: &Event) {
    match event {
        Event::Opened { conn_id, .. } => world.opened(*conn_id),
        Event::Closed { .. } => {}
        Event::Desync(desync) => world.invalidate(desync.conn_id, desync.reason.clone()),
        Event::Decoded(decoded) => match &decoded.packet {
            Packet::ConfigurationClientbound(packet) => {
                world.apply_clientbound_config(decoded.conn_id, packet);
            }
            Packet::PlayClientbound(packet) => {
                // Without an explicit `--follow-conn`, follow the newest
                // connection that reaches Play. The Play login is the first
                // packet of that state, so no world packet is missed.
                if decoded.packet.name() == "login" && *followed != Some(decoded.conn_id) {
                    world.follow(decoded.conn_id);
                    *followed = Some(decoded.conn_id);
                }
                world.apply_clientbound_play(decoded.conn_id, packet);
            }
            Packet::PlayServerbound(packet) => {
                world.apply_serverbound_play(decoded.conn_id, packet);
            }
            _ => {}
        },
    }
}

fn decode(path: &Path) -> CaptureResult {
    let file = File::open(path).unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
    let reader = Reader::new(BufReader::new(file))
        .unwrap_or_else(|e| panic!("read header {}: {e}", path.display()));
    let mut decoder = Decoder::new();
    // Whole-capture replay: wait long enough that every out-of-order record
    // the old proxy wrote is decoded in timestamp order.
    decoder.set_reorder_window(60_000_000_000);
    let mut result = CaptureResult::default();

    for record in reader {
        let record = record.unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        for event in decoder.feed(&record) {
            if let Event::Decoded(decoded) = &event {
                result.packets += 1;
                *result.by_group.entry((decoded.state, decoded.dir)).or_insert(0) += 1;
            }
            if let Event::Desync(desync) = &event {
                result.desyncs.push(format!(
                    "connection {} at {}: {}",
                    desync.conn_id, desync.ts_ns, desync.reason
                ));
            }
            apply(&mut result.world, &mut result.followed, &event);
        }
    }
    for event in decoder.finish() {
        if let Event::Decoded(decoded) = &event {
            result.packets += 1;
            *result.by_group.entry((decoded.state, decoded.dir)).or_insert(0) += 1;
        }
        if let Event::Desync(desync) = &event {
            result.desyncs.push(format!(
                "connection {} at {}: {}",
                desync.conn_id, desync.ts_ns, desync.reason
            ));
        }
        apply(&mut result.world, &mut result.followed, &event);
    }
    for stats in decoder.stats().values() {
        result.failures += stats.decode_failures.values().sum::<u64>();
        result.frames += stats.frames;
        result.bytes += stats.bytes;
    }
    result
}

#[test]
fn captures_decode_with_zero_desyncs() {
    let files = captures();
    if files.is_empty() {
        return;
    }

    let mut problems = Vec::new();
    for path in files {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let result = decode(&path);
        let mut groups = String::new();
        for ((state, dir), count) in &result.by_group {
            groups.push_str(&format!(" {state}/{dir}={count}"));
        }
        eprintln!(
            "{}: {} packets from {} frames / {} bytes, {} desyncs, {} decode failures;{}",
            name,
            result.packets,
            result.frames,
            result.bytes,
            result.desyncs.len(),
            result.failures,
            groups
        );
        if !result.desyncs.is_empty() {
            problems.push(format!("{} desynchronised: {:?}", path.display(), result.desyncs));
        }
        if result.failures != 0 {
            problems.push(format!("{} had decode failures", path.display()));
        }
        if result.packets == 0 {
            problems.push(format!("{} decoded no packets", path.display()));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// One block truth line, expanded over a range.
#[derive(Clone, Debug)]
struct TruthEntry {
    pos: BlockPos,
    name: String,
    properties: Vec<(String, String)>,
}

#[derive(Debug)]
struct TruthFile {
    path: PathBuf,
    entries: Vec<TruthEntry>,
}

fn parse_range(token: &str) -> (i32, i32) {
    match token.split_once("..") {
        Some((start, end)) => (
            start.parse().unwrap_or_else(|_| panic!("bad range start {token}")),
            end.parse().unwrap_or_else(|_| panic!("bad range end {token}")),
        ),
        None => {
            let value = token.parse().unwrap_or_else(|_| panic!("bad coordinate {token}"));
            (value, value)
        }
    }
}

fn parse_block(spec: &str) -> (String, Vec<(String, String)>) {
    let Some(open) = spec.find('[') else {
        return (normalize(spec), Vec::new());
    };
    let name = normalize(&spec[..open]);
    let body = spec[open + 1..].trim_end_matches(']');
    let properties = body
        .split(',')
        .filter(|part| !part.trim().is_empty())
        .map(|part| {
            let (key, value) = part.split_once('=').unwrap_or_else(|| panic!("bad property {part}"));
            (key.trim().to_owned(), value.trim().to_owned())
        })
        .collect();
    (name, properties)
}

fn normalize(name: &str) -> String {
    if name.contains(':') { name.to_owned() } else { format!("minecraft:{name}") }
}

fn parse_truth(path: &Path) -> TruthFile {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    // The files are a command log, not a final snapshot: a later line for the
    // same position supersedes an earlier one (e.g. `stone` then `air (stone
    // removed)`). Keeping the last line is the spike's tolerance rule.
    let mut expected = BTreeMap::new();
    for (line_number, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // A trailing "(...)" note explains the line; it is not data.
        let line = line.split(" (").next().unwrap().trim();
        let (main, except) = match line.split_once(" except ") {
            Some((main, except)) => (main.trim(), Some(except.trim())),
            None => (line, None),
        };
        let fields: Vec<&str> = main.split_whitespace().collect();
        assert!(
            fields.len() >= 4,
            "{}:{}: expected 'X Y Z block'",
            path.display(),
            line_number + 1
        );
        let (x0, x1) = parse_range(fields[0]);
        let (y0, y1) = parse_range(fields[1]);
        let (z0, z1) = parse_range(fields[2]);
        let (name, properties) = parse_block(fields[3]);
        let except = except.map(|text| {
            let coordinates: Vec<i32> =
                text.split_whitespace().map(|value| value.parse().unwrap()).collect();
            (coordinates[0], coordinates[1], coordinates[2])
        });

        for x in x0..=x1 {
            for y in y0..=y1 {
                for z in z0..=z1 {
                    if except == Some((x, y, z)) {
                        continue;
                    }
                    expected.insert((x, y, z), (name.clone(), properties.clone()));
                }
            }
        }
    }
    let entries = expected
        .into_iter()
        .map(|((x, y, z), (name, properties))| TruthEntry {
            pos: BlockPos::new(x, y, z),
            name,
            properties,
        })
        .collect();
    TruthFile { path: path.to_path_buf(), entries }
}

fn truth_files() -> Vec<TruthFile> {
    let dir = repo_root().join("spikes/round0");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        eprintln!("SKIP block truth: {} does not exist on this machine", dir.display());
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("blocks-truth") && name.ends_with(".txt"))
        })
        .collect();
    paths.sort();
    let files: Vec<TruthFile> = paths.iter().map(|path| parse_truth(path)).collect();
    if files.is_empty() {
        eprintln!("SKIP block truth: no blocks-truth*.txt in {}", dir.display());
    }
    files
}

/// Compare one truth entry with a world.
///
/// Tolerance rules, matching the spike's block checks:
/// - the block name must match exactly;
/// - every property the truth names must match exactly;
/// - a truth line without `[...]` compares the name only, so orientation
///   properties that the line does not care about do not fail it;
/// - a missing chunk, an unloaded block or an invalid world is a failure, not
///   an "unknown matches".
fn check(world: &World, entry: &TruthEntry) -> Result<(), String> {
    let Some(description) = world.block_description(entry.pos) else {
        return Err(format!(
            "{:?}: no block (chunk not loaded, outside the world, or world invalid)",
            entry.pos
        ));
    };
    if description.name() != entry.name {
        return Err(format!(
            "{:?}: expected {}, got {}",
            entry.pos,
            entry.name,
            description.display()
        ));
    }
    for (key, value) in &entry.properties {
        let found = description
            .properties()
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str());
        if found != Some(value.as_str()) {
            return Err(format!(
                "{:?}: expected {key}={value}, got {found:?} in {}",
                entry.pos,
                description.display()
            ));
        }
    }
    Ok(())
}

/// Truth files whose capture is not in `spikes/captures/`. `blocks-truth2.txt`
/// was written at 13:02 and its capture slot (`corpus2.prxcap`, 13:03) was then
/// overwritten by the E1 status-ping run, which has no world at all.
const UNMATCHED_TRUTH: &[(&str, &str)] = &[(
    "blocks-truth2.txt",
    "its capture was overwritten by the E1 status-ping run (corpus2.prxcap)",
)];

#[test]
fn block_truth_matches_a_capture() {
    let truths = truth_files();
    if truths.is_empty() {
        return;
    }
    let files = captures();
    if files.is_empty() {
        return;
    }

    let mut matched: BTreeSet<usize> = BTreeSet::new();
    for path in &files {
        let result = decode(path);
        for (index, truth) in truths.iter().enumerate() {
            if matched.contains(&index) {
                continue;
            }
            let mut failure = None;
            for entry in &truth.entries {
                if let Err(message) = check(&result.world, entry) {
                    failure = Some(message);
                    break;
                }
            }
            match failure {
                None => {
                    matched.insert(index);
                    eprintln!(
                        "{}: {} entries match {}",
                        truth.path.file_name().unwrap().to_string_lossy(),
                        truth.entries.len(),
                        path.file_name().unwrap().to_string_lossy()
                    );
                }
                Some(message) => {
                    eprintln!(
                        "{}: not {} ({message})",
                        truth.path.file_name().unwrap().to_string_lossy(),
                        path.file_name().unwrap().to_string_lossy()
                    );
                }
            }
        }
    }

    let mut missing = Vec::new();
    for (index, truth) in truths.iter().enumerate() {
        if matched.contains(&index) {
            continue;
        }
        let name = truth.path.file_name().unwrap().to_string_lossy().into_owned();
        if let Some((_, reason)) = UNMATCHED_TRUTH.iter().find(|(file, _)| *file == name) {
            eprintln!("{name}: no capture on this machine ({reason})");
        } else {
            missing.push(truth.path.display().to_string());
        }
    }
    assert!(missing.is_empty(), "no capture matched: {missing:?}");
}
