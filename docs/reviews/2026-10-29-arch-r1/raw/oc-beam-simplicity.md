<!-- Reviewer: Beam Free via OpenCode opencode v2.1.2. Lens: beam-simplicity. Raw, unverified output. -->

### Findings

- **ID**: SC-1
  - **Severity**: major
  - **Where**: `docs/architecture.md` §1, §4 (Curriculum), §11; `docs/sessions/10-autonomy.md`
  - **Claim**: The architecture sets the capability target at "the whole game up to the Ender Dragon" and a free Voyager-style curriculum for the first demo, far beyond the agreed first demo (fresh survival world, one verified chat command, first night).
  - **Evidence**: architecture §1 "capability target is the whole game up to the Ender Dragon"; first demo in decisions 2026-10-23 is hand-over + verified command + first night. Session 10 chose free curriculum over ladder spine.
  - **Recommendation**: Scope the first demo to a single verified command and surviving the first night; defer curriculum, project ledger, announcements and stronghold finding until the body+brain loop works.
  - **Confidence**: high

- **ID**: SC-2
  - **Severity**: major
  - **Where**: `docs/architecture.md` §6; `docs/sessions/11-skill-learning.md`
  - **Claim**: The staged admission pipeline (practice room on a test server, k=3 runs, probation, git versions, rollback, trust zones, red-team) is designed before any LLM-written skill exists; the first demo rests on hand-written core skills.
  - **Evidence**: Session 11 admission stages; architecture §6 "practice room (a copy of the world on a test server, three runs...)".
  - **Recommendation**: Ship hand-written core skills only for the first demo; implement the admission gate when the first LLM skill is actually attempted.
  - **Confidence**: high

- **ID**: SC-3
  - **Severity**: major
  - **Where**: `docs/architecture.md` §4 (Wiki RAG), §8; `docs/sessions/12-memory-rag.md`
  - **Claim**: A local Minecraft Wiki index with Qwen3 embeddings and a reranker adds a model download, Windows dependency, versioning and an unresolved CC BY-NC-SA licence conflict, while game data already comes from Mojang reports and the LLM has Minecraft knowledge.
  - **Evidence**: Session 12 "CC BY-NC-SA, which needs attribution and non-commercial use"; architecture §8 "index is never distributed".
  - **Recommendation**: Drop from the first demo; use Mojang data + LLM knowledge; revisit RAG only if measured knowledge gaps appear.
  - **Confidence**: high

- **ID**: SC-4
  - **Severity**: major
  - **Where**: `docs/architecture.md` §3, §4; `docs/sessions/08-jev-llm-brain.md`; `docs/research/decision-models.md`
  - **Claim**: Jev's value is unproven (decision-models.md notes hand-written controllers often beat models in System1Doom), yet wiring it into the first demo adds latency budgets, calibration, stamped directives and fallback logic for uncertain benefit.
  - **Evidence**: decision-models.md "In most scenarios the hand-written controller dominated"; architecture risk #5 "Jev's value is unproven".
  - **Recommendation**: Keep Jev behind a flag; first demo uses rules + LLM only; add Jev at decision points only where measurement shows benefit, keeping the backend-agnostic client.
  - **Confidence**: medium

- **ID**: SC-5
  - **Severity**: minor
  - **Where**: `docs/architecture.md` §4 (World model: "provenance on every fact"); `docs/sessions/03-perception-world-model.md` (minor picks)
  - **Claim**: Provenance tags on every fact and push invalidation add fields and handlers with no demo query, and contradict the simplicity review that already recommended dropping them.
  - **Evidence**: critic-simplicity.md §3.3 drops `source`/`confidence`/`superseded_by`; §3.9 drops push invalidation; session 3 still lists both.
  - **Recommendation**: Reconcile: drop provenance tags (single write API enforces fair play), drop push invalidation (snapshot on "Unload Chunk" if needed later); keep age + source.
  - **Confidence**: high

- **ID**: SC-6
  - **Severity**: minor
  - **Where**: `docs/sessions/03-perception-world-model.md` (minor picks); `docs/research/systems-review.md` (Baritone)
  - **Claim**: Keeping a compact 2-bits-per-block chunk cache of unloaded chunks adds memory and snapshot logic with no demo travel exceeding view distance, contradicting the "smallest memory" decision.
  - **Evidence**: Session 3 "Remembered blocks of unloaded chunks are kept as a compact chunk cache"; session 12 triggers stage E only "when the demo's travel distance regularly exceeds view distance".
  - **Recommendation**: Drop for the first demo; plan from loaded chunks only; add only if measurements require it.
  - **Confidence**: high

- **ID**: SC-7
  - **Severity**: minor
  - **Where**: `docs/architecture.md` §9; `docs/sessions/14-testing.md`
  - **Claim**: Level 2 protocol test body plus a practice room on a test server duplicate level 3 real-body coverage and add fixture-restore complexity; the first demo can validate with level 1 + level 3.
  - **Evidence**: Session 14 three levels; session 11 "fixture restore time (above 30 s means a nightly batch)".
  - **Recommendation**: Keep level 1 (fake world) and level 3 (real body); defer level 2 / protocol body and practice room until LLM skills need safe gating.
  - **Confidence**: medium

- **ID**: SC-8
  - **Severity**: minor
  - **Where**: `docs/architecture.md` §4 (Movement style); `docs/sessions/06-movement.md`, `docs/sessions/13-natural-feel.md`
  - **Claim**: A separate movement-style layer with `human.js` calibration and a blind test adds tuning burden; the first demo only needs functional movement and survival.
  - **Evidence**: Session 6 "Natural movement is a separate layer above the planner"; session 13 blind test with OBS recordings.
  - **Recommendation**: Use basic planner movement for the first demo; add the human-like style layer only after baseline measurements.
  - **Confidence**: medium

- **ID**: SC-9
  - **Severity**: minor
  - **Where**: `docs/architecture.md` §4 (Interface window); `docs/sessions/02-body-connection.md` §2.4
  - **Claim**: The interface lists five features (indicator, reports, command box, live view, memory viewer) with undecided technology; live view and memory viewer are not needed for the first demo.
  - **Evidence**: Architecture §4 "Interface window | not decided [O]"; §4 row lists all five.
  - **Recommendation**: First demo: small always-on-top indicator + text command box; reports to file/console; defer live view and memory viewer.
  - **Confidence**: high

- **ID**: SC-10
  - **Severity**: minor
  - **Where**: `docs/architecture.md` §2; `docs/sessions/02-body-connection.md` §2.4
  - **Claim**: A separate watchdog OS process adds process management; an in-process cleanup path may suffice unless experiments show otherwise.
  - **Evidence**: Session 2.4 "A tiny watchdog process releases all injected keys if the main process dies, because Windows does not."
  - **Recommendation**: Try in-process cleanup first; promote to a separate process only if experiments require it.
  - **Confidence**: low

- **ID**: SC-11
  - **Severity**: minor
  - **Where**: `docs/sessions/03-perception-world-model.md` (minor picks)
  - **Claim**: Sound-packet hearing within 16 blocks adds a perception subsystem with no measured need for first-night survival.
  - **Evidence**: Session 3 "Hearing counts as a sense: sound packets carry a position and are used within about 16 blocks."
  - **Recommendation**: Drop from the first demo; add only if a measured scenario needs it.
  - **Confidence**: medium

- **ID**: SC-12
  - **Severity**: minor
  - **Where**: `docs/architecture.md` §10
  - **Claim**: Splitting the body into ~12 crates before any code exists is speculative scaffolding; the valuable part is the layering rule, not the crate split.
  - **Evidence**: Architecture §10 lists 12 body modules; §10 "Directory names are proposed".
  - **Recommendation**: Start with one Rust crate (body) + one Python package; split only when a module boundary proves needed; keep the import rule enforced by tooling.
  - **Confidence**: high

### Ideas (brainstorm)

1. Collapse the skill compiler's guard derivation to hand-written guards for the first 3-5 core skills; derive guards only once LLM-written skills arrive — why: removes a compiler subsystem from the critical path.
2. Run the first demo with a single append-only log as the only persistence, folding at load, and no SQLite/vectors/lessons — why: matches the "smallest memory" decision and keeps the file hand-readable.
3. Replace Jev with a simple rules table plus the LLM for the first demo, keeping the `/v1/systemone` client interface swappable — why: removes an unproven hosted dependency from the decision path.
4. Make the Azalea decoder risk explicit by writing a minimal decoder for only the packets the first demo needs (movement, blocks, inventory, chat) as the fallback plan — why: bounds the E2/E3 experiment risk.
5. Defer perception profiles and ablations; ship one fair-play profile — why: removes configurable profile machinery with no first-demo measurement.
6. Defer the memory viewer and live view; expose state via the replayable log and a simple CLI dump — why: removes an undecided UI technology from the critical path.
7. Keep the body port but allow an in-process mode for level-1 tests — why: simpler dev loop without changing the contract.

### Questions for the user

1. May the first demo be scoped to "first night + one verified command" with no curriculum or autonomy?
2. Should the wiki RAG be dropped from the first demo despite the earlier decision, given the licence conflict?
3. Is Jev allowed to be optional/flagged in the first demo pending measurement?
4. Should the skill admission gate be deferred until the first LLM-written skill is attempted?
5. Should the level-2 protocol test body be dropped initially, keeping level 1 + level 3?

FINAL_REPORT The top three findings are that the first demo is over-scoped to a whole-game capability target with a free curriculum (SC-1), the skill admission pipeline is built before any LLM-written skill exists (SC-2), and the wiki RAG with embeddings adds heavy unverified machinery and an unresolved licence conflict (SC-3); cutting these three removes most of the speculative scope while preserving the hard rules (real client, zero cheats, user take-over, skills written while playing).
