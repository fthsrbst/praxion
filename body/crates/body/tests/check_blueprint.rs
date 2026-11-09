//! `praxion-body check-blueprint` end to end: the one JSON object the brain's
//! repair loop reads, the exit codes (0 accepted, 1 rejected, 2 bad input) and
//! the `--emit-plan` dump the referee uses instead of re-parsing the format.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{Value, json};

fn fixture(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../blueprints")
        .join(path)
}

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_praxion-body"))
        .args(args)
        .output()
        .expect("run praxion-body")
}

fn json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout is not JSON: {error}\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

#[test]
fn an_accepted_blueprint_prints_one_json_object_and_exits_zero() {
    let fallback = fixture("fallback_shelter.bp");
    let output = run(&[
        "check-blueprint",
        fallback.to_str().unwrap(),
        // A namespaced id must normalize to the short form the palette stores.
        "--inventory",
        r#"{"minecraft:cobblestone": 74, "torch": 1}"#,
    ]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");

    let verdict = json(&output);
    assert_eq!(verdict["ok"], true);
    assert_eq!(verdict["file"], fallback.display().to_string());
    assert_eq!(verdict["blueprint"], "fallback_shelter");
    assert_eq!(verdict["anchor"], json!([0, 0, 0]));
    assert_eq!(verdict["size"], json!([5, 5, 4]));
    assert_eq!(verdict["placements"], 75);
    assert_eq!(verdict["materials"]["cobblestone"], 74);
    assert_eq!(verdict["materials"]["torch"], 1);
    assert_eq!(verdict["warnings"], json!([]));
    assert!(verdict.get("plan").is_none());
}

#[test]
fn a_short_inventory_prints_the_typed_failure_and_exits_one() {
    let fallback = fixture("fallback_shelter.bp");
    let output = run(&[
        "check-blueprint",
        fallback.to_str().unwrap(),
        "--inventory",
        r#"{"cobblestone": 73, "torch": 1}"#,
        "--anchor",
        "112",
        "64",
        "-38",
    ]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");

    let verdict = json(&output);
    assert_eq!(verdict["ok"], false);
    assert_eq!(verdict["kind"], "short_materials");
    assert_eq!(verdict["block"], "cobblestone");
    assert_eq!(verdict["need"], 74);
    assert_eq!(verdict["have"], 73);
    assert!(
        verdict["message"]
            .as_str()
            .unwrap()
            .contains("cobblestone: need 74, have 73")
    );
    // The cells are world positions for the anchor in the same object.
    let cells = verdict["cells"].as_array().unwrap();
    assert_eq!(cells.len(), 74);
    assert!(cells.contains(&json!([112, 64, -38])));
}

#[test]
fn emit_plan_gives_the_referee_world_positions() {
    let fallback = fixture("fallback_shelter.bp");
    let output = run(&[
        "check-blueprint",
        fallback.to_str().unwrap(),
        "--inventory",
        r#"{"cobblestone": 74, "torch": 1}"#,
        "--emit-plan",
        "--anchor",
        "112",
        "64",
        "-38",
    ]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");

    let verdict = json(&output);
    assert_eq!(verdict["anchor"], json!([112, 64, -38]));
    let plan = verdict["plan"].as_array().unwrap();
    assert_eq!(plan.len(), 75);
    assert_eq!(plan[0]["cell"], json!([0, 0, 0]));
    assert_eq!(plan[0]["pos"], json!([112, 64, -38]));
    assert_eq!(plan[0]["block"], "cobblestone");
    assert_eq!(plan[0]["kind"], "solid");

    let torch = plan
        .iter()
        .find(|placement| placement["kind"] == "torch")
        .expect("the fallback has a torch");
    assert_eq!(torch["cell"], json!([1, 1, 1]));
    assert_eq!(torch["pos"], json!([113, 65, -37]));

    let entry = plan
        .iter()
        .find(|placement| placement["kind"] == "entry")
        .expect("the fallback has an entry");
    assert_eq!(entry["cell"], json!([4, 0, 2]));
}

#[test]
fn a_rejected_design_reports_the_kind_cells_and_anchor() {
    let draft = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../blueprint/tests/fixtures/draft_example.bp");
    let output = run(&[
        "check-blueprint",
        draft.to_str().unwrap(),
        "--anchor",
        "10",
        "20",
        "30",
    ]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");

    let verdict = json(&output);
    assert_eq!(verdict["ok"], false);
    assert_eq!(verdict["kind"], "torch_floating");
    assert_eq!(verdict["cells"], json!([[12, 21, 31]]));
    assert_eq!(verdict["blueprint"], "draft_example");
}

#[test]
fn an_unreadable_file_is_exit_two_not_a_verdict() {
    let output = run(&["check-blueprint", "no/such/design.bp"]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("cannot read"),
        "{output:?}"
    );
}

#[test]
fn a_bad_inventory_is_exit_two_not_a_verdict() {
    let fallback = fixture("fallback_shelter.bp");
    let output = run(&[
        "check-blueprint",
        fallback.to_str().unwrap(),
        "--inventory",
        "not json",
    ]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("--inventory wants"),
        "{output:?}"
    );
}
