//! `--summary-json` on a real capture: the inventory and entity-type maps an
//! oracle needs to compare with the referee.
//!
//! Captures are machine-local and ignored by git, so a machine without them
//! skips with an explicit message instead of failing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

/// The capture to test, best first: the 5a end-to-end capture has known
/// contents, the corpus captures only prove the maps are consistent.
fn capture() -> Option<PathBuf> {
    let mut candidates = vec![repo_root().join("logs/t5a/e2e.prxcap")];
    let corpus = repo_root().join("spikes/captures");
    if let Ok(entries) = std::fs::read_dir(&corpus) {
        let mut files: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "prxcap"))
            .collect();
        files.sort();
        candidates.extend(files);
    }
    candidates.into_iter().find(|path| path.is_file())
}

/// The object following `"key":` in a JSON document. Test-only: the summary
/// has no nested object under either key, only string keys to numbers.
fn object_field<'a>(json: &'a str, key: &str) -> &'a str {
    let needle = format!("\"{key}\":");
    let start = json
        .find(&needle)
        .unwrap_or_else(|| panic!("summary has no {needle}"))
        + needle.len();
    let rest = &json[start..];
    assert!(rest.starts_with('{'), "summary {key} is not an object: {rest}");
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    for (index, ch) in rest.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &rest[1..index];
                }
            }
            _ => {}
        }
    }
    panic!("summary {key} object is not balanced: {rest}");
}

fn parse_counts(object: &str) -> BTreeMap<String, u64> {
    object
        .split(',')
        .filter(|part| !part.trim().is_empty())
        .map(|part| {
            // The key itself contains a colon (`minecraft:oak_log`), so split at
            // the last one.
            let (key, value) = part
                .rsplit_once(':')
                .unwrap_or_else(|| panic!("not a count pair: {part}"));
            (
                key.trim().trim_matches('"').to_owned(),
                value.trim().parse().unwrap_or_else(|_| panic!("bad count in {part}")),
            )
        })
        .collect()
}

fn top_level_number(json: &str, key: &str) -> u64 {
    let needle = format!("\"{key}\":");
    let start = json
        .find(&needle)
        .unwrap_or_else(|| panic!("summary has no {needle}"))
        + needle.len();
    let digits: String = json[start..]
        .chars()
        .skip_while(|ch| ch.is_whitespace())
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().unwrap_or_else(|_| panic!("summary {key} is not a number"))
}

#[test]
fn summary_has_inventory_and_entity_type_histograms() {
    let Some(path) = capture() else {
        eprintln!("SKIP summary: no capture in logs/t5a or spikes/captures on this machine");
        return;
    };
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    let out = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("summary-{name}.json"));
    let output = Command::new(env!("CARGO_BIN_EXE_praxion-decode"))
        .arg(&path)
        .arg("--summary-json")
        .arg(&out)
        .output()
        .unwrap_or_else(|e| panic!("run praxion-decode on {}: {e}", path.display()));
    assert!(
        output.status.success(),
        "praxion-decode {} failed:\n{}\n{}",
        path.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let json = std::fs::read_to_string(&out)
        .unwrap_or_else(|e| panic!("read {}: {e}", out.display()));
    let _ = std::fs::remove_file(&out);

    let inventory = parse_counts(object_field(&json, "inventory"));
    let entity_types = parse_counts(object_field(&json, "entity_types"));

    // The histogram must cover exactly the tracked entities.
    let entities = top_level_number(&json, "entities");
    assert_eq!(
        entity_types.values().sum::<u64>(),
        entities,
        "entity_types does not add up to the tracked entities in {name}"
    );
    assert!(entity_types.values().all(|count| *count > 0));
    assert!(inventory.values().all(|count| *count > 0));

    if name == "e2e.prxcap" {
        // Fixtures applied by logs/t5a/e2e.py: oak_log x5, a pickaxe and a
        // zombie. Natural mobs are enabled too, so the zombie count is at
        // least the summoned one.
        assert_eq!(inventory.get("minecraft:oak_log"), Some(&5));
        assert_eq!(inventory.get("minecraft:stone_pickaxe"), Some(&1));
        let zombies = entity_types.get("minecraft:zombie").copied().unwrap_or(0);
        assert!(zombies >= 1, "no zombie in the entity histogram");
    } else {
        eprintln!(
            "{name}: {} inventory item types, {} entity types",
            inventory.len(),
            entity_types.len()
        );
    }
}
