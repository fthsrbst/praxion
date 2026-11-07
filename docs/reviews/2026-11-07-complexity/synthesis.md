# Complexity audit, 2026-11-07

Trigger: the user finds the code too complicated and wants minimal, clean code with minimal debt.
Auditors (read-only): GLM-5.3-Flash (behaviour crates, `a1`), DeepSeek v4.1 Flash (repository structure, `a2`).
Raw reports: `logs/audit-1107/a1-simplicity.answer.txt`, `logs/audit-1107/a2-structure.answer.txt` (ignored).
Orchestrator (Claude Opus 5.5) re-checked the items marked **verified**.

## Baseline

- `body/`: 19 crates, ~34k source lines, ~22k test lines, 623 tests. `cargo test --workspace`: all binaries pass.
- No CI, no clippy config; `cargo clippy` is not installed for the pinned nightly.
- Largest file: `primitives/src/runner.rs` 3353 lines (Explore::tick 294 lines, Craft::tick_inner 218).

## Findings, ranked

1. **Dead code paths (verified).** `fakeworld` crate (1.9k lines) has no consumer. `praxion-live` /
   `LiveDriver` (`primitives/src/live.rs` + `bin/live.rs`, ~980 lines) has no consumer and is an older copy
   of `body/src/driver.rs`. `praxion-decode` bin (530 lines): consumer unknown.
2. **Duplicated driver has drifted (verified).** `BridgeActuator` exists twice; only the body copy clears a
   stale outcome on `start`. The dig-before-registration fix (`with_observed_block` on Dig) is in
   `live.rs:187` but missing in `body/src/driver.rs` (product path). Real latent bug.
3. **God files/functions.** `runner.rs` mixes ten primitives plus frontier ranking; `Interpreter::select`
   269 lines; settle/tick policy duplicated in `Runner`.
4. **Debug `eprintln!` and seed-specific probes in product paths** (`runner.rs`, `live.rs`, `headless`).
5. **Test fixture copies:** ~1.8k lines copied between `src/tests.rs` and `tests/independent.rs`.
6. **Cargo noise:** 21 unused or misplaced dependency entries; 19 `pub` items with no use.
7. **Small duplicates:** `normalize` x5, item counting x3, `PLAYER_STORAGE_SLOTS`, `section_block_pos`, `POLL`.
8. **Raw-store boundary not honest:** architecture §4/§5.1 says only perception reads the raw store, with
   a lint; primitives, receipts, reflexes, body read `World` directly and there is no lint. Needs a decision.
9. **Python:** `setup.py` duplicates harness manifest and server.properties code; it never builds `body/`.
10. **Stale docs:** README "no code yet", architecture §10 crate list, SETUP "never depends on spikes",
    backlog "Step 9 follow-ups" (done). `.agents/skills` and `.claude/skills` are two drifted copies.
11. **spikes/:** 3 GB of `target/`; `spikes/proxy` superseded by the forwarder.

## Process finding

An orphan OpenCode session (`s9d2-resume`, created 2026-11-02, no client) was still editing the tree on
2026-11-07 (runner.rs, docs/rule-lists.md, notes.md). Session export: `logs/audit-1107/s9d2-resume.session.json`.
