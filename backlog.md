# Backlog

Unfinished work left by agents and subagents. If you stop with something incomplete,
it goes here before the session ends. Remove an item (or move it to "Done") once it is finished and verified.

**Depth rule:** keep each entry short here. When an item needs more than ~15 lines
(investigation log, sub-tasks, findings), move the detail to `backlog/<slug>.md`
and leave a one-line link. Split further (`backlog/<slug>/<part>.md`) the same way if needed.

**Entry format**

```md
### <title>
- Source: <agent/model>, <YYYY-MM-DD>
- State: <what is done>
- Remaining: <what is left, concretely>
- Pointers: <files, branches, logs, links>
```

## Open

### Survival exemption in the need gate (2026-11-09, smarter play slice 1)
- Source: review `logs/mac1109/review-smarter.answer.txt` finding 10
- State: while a covered rung waits for its skill, every proposal that serves no later rung is refused `not_needed`
  (`brain/praxion_brain/needs.py`, the `waiting` rule), so a night/mob survival goal is refused too.
- Remaining: when the survival slice lands, exempt health/food goals (and sheltering) from the waiting rule.
- Pointers: `brain/praxion_brain/needs.py`, `docs/decisions.md` 2026-11-09.

### Simplification pass, then movement style (2026-11-07 plan)
- Source: Claude Opus 5.5, 2026-11-07 (`docs/decisions.md` 2026-11-07)
- State: A1-A4 done, A5 fixtures done (forwarder, receipts, perception, interpreter); movement style
  part 1 (`Gaze`, 15e512d) and part 2 (walking: string-pulled route, pursuit carrot, deadband
  steering, sprint; briefs `logs/simplify/c2-walk.md`, `c2b-fix.md`) done.
- A5 clippy and small duplicates done (brief `logs/simplify/a5-clippy.md`; 8 justified `allow`s).
- Live run 2026-11-07 (seed 303, `get_stone_pickaxe`, run `runs/test-body-8036648a954f4c128a6d9645bbb6eb73`):
  done in about 1300 play ticks, 18/18 receipts confirmed, no reflexes, no deaths, no place timeouts,
  peak yaw 28.8 deg/tick (no snaps). Owner's visual verdict pending.
- Remaining: compare against human traces (research §6); idle gaze and pitch noise (research §4.6); one walkthrough for
  the user.
- Walking limits: diagonal A* steps are not string-pulled (a naive version broke the narrowing
  corridor test); the hazard window measures to a step's end, so a Descend edge can sit up to one block
  inside the 4.0 window before it counts.
- Thin Demo-2 LLM/Jev slice (2026-11-08): moved to a separate session by the user. Proposed design
  (not yet agreed): `brain/` Python package; Jev maps a goal to a known skill or "new skill needed";
  an OpenAI model (gpt-6.1-terra vs gpt-6.1-sol, measured) writes a `.skill` into `skills/candidates/`;
  admission = new body flag `--check-skills` + k=3 practice runs on a fresh-seed harness server with
  a referee checking the effect; first target `get_furnace`; no body port in the slice (deviation to approve).
- Watcher signalled 2026-11-08 for the movement work (`logs/WATCH_FROM` e6ac44c .. ca9bf47+); the
  watcher's review covers the unreviewed explore and `known.rs` edits.
- Explore frontier strategy (2026-11-08): with the view scan live (debug `view scan` lines show rays
  reaching 64 blocks), seed 303 spawns where a hill hides the tree still fail `explore #logs` at 1200
  ticks in about 2 of 5 runs: the nearest-frontier order wanders downhill and west. A person would climb
  for a view or follow open ground. Needs a design (score by height or by unseen sky area); runs
  `runs/test-body-405d2ee3274444ae8e1c75b959a749fb`, `...11b45b28...` (3 follower stalls at deviation 0.8).
- Explore review fixes (2026-11-08, `logs/review/r1108/`): the "at most 2 cue attempts on a known canopy"
  rule has no behavioural test; a test needs a `World` fixture with chunks so `seek` runs on real known
  leaves (same fixture as "Mine and HandSelect tick paths lack tests"). The worker's counter-only test was
  removed as tautological. The same fixture is needed for tests of the walk-back cap (`EXPLORE_MAX_WALK_BACKS`),
  the empty walk-back/frontier plan skips, the look-around ending when a target is found mid-turn, and
  `open_to_sky` against a real heightmap (GLM review `logs/review/r1108/r-explore-fixes.answer.txt` Q6).
- NEXT (live): run 6 to verify `find_ore`/`branch_mine` (commit 7ab0ef5) and the two loop fixes (c1b7405 side goals
  while a skill is written, f1be00e no repeat of the newest goal) on a fresh seed; target coal and raw iron. Run 5
  (seed 505) reached the furnace with the self-written `get_furnace`, then stopped at coal (`inconclusive_body`, no
  ore on the surface) and idled (the two loop bugs). Command: `cd harness && python -m praxion_harness.autonomy
  --seed 606 --minutes 45` (Windows; body release build from HEAD first).
- branch_mine leftovers (GLM review `logs/oc-review-ore/brief.answer.txt` finding 5): no Digger-level test of the
  real route (stone world descends then tunnels; ore ahead -> Confirmed before the next dig; dig cap -> not_found;
  dry-step limit -> stuck). The DeepSeek worker was cut off before it wrote them. Use the `primitives/tests/
  independent.rs` harness.
- `cargo test --workspace` not run to completion after the dig_down rewrite: the decoder `corpus.rs` and reflexes
  `matrix.rs` tests take 30+ minutes; primitives, rules, interpreter (228 tests) and clippy pass.
- The repo is not rustfmt-clean: `cargo fmt` touches ~105 unrelated files. Those changes sit in
  `git stash@{0}` ("cargo fmt noise ...") on the Windows machine only. Decide: one dedicated fmt commit, or drop
  the stash. Until then, never run `cargo fmt` in a worker brief.
- NEXT (body, after run 6): a `smelt` primitive (furnace menu: input, fuel, wait, take output); gates the
  iron_ingot rung. Menu clicks already exist (`craft.rs`, `MenuAction`).
- `craft` vs `near(crafting_table, 4)` disagree (autonomous run 2, seed 202,
  `runs/test-body-774ebe1d772149d8b028db880afe62eb`): `get_stone_sword` chose its craft rule (near true) and `craft`
  failed `needs_table` after 195 ticks. Same class as wet stone (near vs mine): give craft and near one predicate
  (the table known within 4 and reachable to click), or have craft walk until it sees the table.
- Watch: the `place` registry budget (80 ticks) is tight with human aiming (GLM review, about 74-81
  ticks worst case); check for place timeouts in the first live runs.
- Decision owed (user): the raw-store/perception boundary in architecture §4/§5.1 is not enforced in
  code; either enforce it or update the doc.
- Unverified carry-over: the orphan worker `s9d2-resume` added an "explore waits while airborne before
  planning" guard (`runner.rs`, Explore::tick). Unit tests pass; no live run yet. Check it with the
  bad-spawn explore item below.

### End-to-end skills from a free spawn
- Source: OpenCode DeepSeek V4.1 Flash, 2026-11-02; updated Claude Opus 5.5, 2026-11-03 (macOS)
- State: 2026-11-03 landed table carry, `mine` tool selection, one-shot swaps, per-run `ExploreMemory`
  (0367d23, e447391) and the place/craft click fixes (be74ba0: body-box overlap, a current bounding
  box, true-centre aim, pick verification, plants cleared). Fresh-spawn `get_stone_pickaxe` from the
  seed-303 spawn `5.5,97,44.5` **finished twice** (3978 and 4035 ticks, referee-confirmed: recorded in
  `runs/video/`). Spawn points vary between runs, and from other spawns explore never finds a log:
  303 twice, 101 twice (explore stops choosing frontiers, e.g. near `-260,66,-102` at tick ~1180).
- Remaining, in order: (1) **explore from a bad spawn** (the WIP's unfinished "peak fix"): explore may
  not change the world, so a body in a pit or behind a 3-block rim has no frontier; reproduce from the
  kept 101 run, then decide with the user (allow pillar/dig moves in explore, or a stuck recovery);
  (2) **known-map lag after a place**: right after `place(crafting_table)` confirmed, `near(table, 4)`
  stayed false long enough that the skill explored for logs and crafted a second table (303 video run,
  ticks 3382-3862, ~500 ticks lost); the place receipt could write the placed cell into the known map;
  (3) rerun 101/202/303 from fresh spawns, then make the runner a `-m server` harness test (today it is
  a 20-line `start_body(seed, "get_stone_pickaxe", setup=peaceful daytime, max_ticks=24000)` loop);
  (4) a 303 run took 2392 s wall for 1499 ticks (period p50 50 ms): find where it stalled outside the loop.
- Pointers: `runs/test-body-e83d1bea80fc485895e6d958a73de976` (303 done), `runs/test-body-6df7c6bd11304710b9656ecc034e9830`
  (101 pit), `notes.md` 2026-11-02 and 2026-11-03.

### Design the thin Demo-2 slice with the user (after Demo-0)
- Source: Claude Opus 5.5, 2026-11-03 (order decided by the user, `docs/decisions.md`)
- State: order decided only: Demo-0, then this slice on the headless body, then Demo-1.
- Remaining: a design session before any `brain/` code. Questions to settle, each with a
  recommendation: which skill the slice targets (one the hand-written set lacks, e.g. `get_iron_pickaxe`
  or `make_shelter`); which model writes it (hosted LLM, and where Jev decides); what admission checks
  (DSL checker, depth and effect checks from the registry, fake-world runs on N seeds, then a referee-
  checked headless run); the `brain/` to body-port interface (architecture §5.4, §5.7); what is logged
  as evidence. Then build in dependency order with review rounds.
- Pointers: architecture §1.1, §5.4, §5.7, §6.3; sessions 8 and 11.

### Melee smoke test is timing-dependent
- Source: Claude Opus 5.5, 2026-11-03 (macOS)
- State: `test_melee_reflex_preempts_and_the_skill_resumes` passed 3 of 5 runs today. Failures: (a) the
  reflex preempted a `craft` mid-way and every later `craft(#planks)` failed `no_result` (items left in
  the 2x2 grid, as the review predicted); (b) the body finished `get_stone_sword` in 124 ticks before
  the zombie arrived, so no reflex fired.
- Remaining: (a) a preempted or failed craft returns grid items to the inventory (or craft clears the
  grid before planning clicks), with a unit test; (b) make the test deterministic: summon the zombie
  in reach and give fewer items so the skill is still running when it arrives.
- Pointers: `runs/test-body-4e8bc891e410490d9ef91286236ce2cd` (a), `harness/tests/test_body_smoke.py`.

### Demo video: spectate lock drops
- Source: Claude Opus 5.5, 2026-11-03 (macOS)
- State: a spectator client (`third_party/minecraft-client/`, assets and libraries sha1-verified,
  scratch launcher and recorder scripts not in the repo) recorded the bot first-person with
  `/spectate Praxion Watcher` and `screencapture -v`. The lock dropped after ~115 s both times and
  re-issuing the same `spectate` every 2 s did not restore it. `screencapture` records the whole screen,
  so other windows enter the video.
- Remaining: find why the lock drops (try `spectate` off then on, or a camera that follows by `tp`),
  record only the Minecraft window, commit the launcher/recorder as a `harness` tool if the user wants it.

### Re-check the crafting-table click under CPU load
- Source: Claude Opus 5.5, 2026-11-03 (macOS)
- State: smoke `test_wooden_pickaxe_from_given_logs[101]` once failed `needs_table` under a parallel
  debug build: Use clicks for 3 x 60 ticks, no menu. The likely cause, `craft` aiming at the table's
  bottom edge, is fixed in be74ba0 (true-centre aim, click only when the pick is on the table).
- Remaining: rerun the smoke suite under the same load to confirm; reopen if it fails.
- Pointers: `runs/test-body-738422ab2b7b4f89a4351a36da5aee99`.

### Mine and HandSelect tick paths lack tests
- Source: OpenCode review, 2026-11-03
- State: `choose_tool` has pure unit tests; the `Mine::tick` paths (`tool_waited` fallback, `Missing`
  through `tick`, no swap once a dig is under way) and `HandSelect` (one swap, wait, resend after 20
  ticks) are only covered live.
- Remaining: a `World` fixture with an inventory in `praxion-primitives` tests and tests for those paths.

### Biome and light in the raw store (F3 facts)
- Source: OpenCode DeepSeek V4.1 Flash, 2026-11-01
- State: `Perception::biome` and `Perception::light_level` now take `&World` and answer
  `Unknown(never_seen)`; the raw store carries neither biome nor light yet (t4 review finding 4).
- Remaining: record biome per chunk section and light per block in `praxion-world` from the
  decoder's chunk and light packets, then answer both from the world and keep
  `Unknown(never_seen)` only when the data is genuinely absent.
- Pointers: `body/crates/perception/src/lib.rs` (`biome`, `light_level` TODOs),
  `body/crates/world/src/lib.rs`, `logs/demo0/t4-perception.review.md` finding 4

### Verify OpenCode safety rails on Windows
- Source: Claude Opus 5.5, 2026-10-31
- State: Rails test ran 2026-10-31 (user-approved). Git commit variants and read/write tools outside the repo are denied.
  Shell commands with absolute Windows paths (`type C:\...`) and `~` paths read outside files: not caught.
- Remaining: agree a fix with the user (shell deny rules for drive-letter paths, or accept and rely on the brief),
  apply it, re-run the test. Answered 2026-10-31: the service DOES keep running a session after
  `oc-run.sh` kills its client (a decoder worker kept editing for 18 min); stop it with
  `opencode session export <id>` then `opencode session delete <id>`. Make `oc-run.sh` do this on timeout.
- Pointers: `.claude/skills/opencode-worker/SKILL.md` ("Safety rails"), `opencode.json`

### Finish the source-level systems review
- Source: Claude Opus 5.5, 2026-10-21
- State: Ten systems read; findings in `docs/research/systems-review.md`.
- Remaining: Craftonomous `src/planning/` and `docs/ARCHITECTURE.md`; VillagerAgent, Plan4MC,
  XENON, ADAM, MineLand, hermescraft, plancraft; mineflayer `blocks.js`/`entities.js`;
  protocol version differences. Do each before the session that needs it.
- Pointers: `docs/research/systems-review.md` §7

### Session 3 leftovers
- Source: Claude Sonnet 5.5, 2026-10-29
- State: Session 3 decided; see `docs/sessions/03-perception-world-model.md`.
- Remaining: object identity, TTLs per fact kind, briefing contract (session 8 and 12); lessons,
  cross-world memory, LLM-written lessons, storage (session 12); check in a 2.7 packet capture
  whether container contents arrive only when opened; transparent blocks in the line-of-sight walk.
- Pointers: `docs/research/memory-review/explainer.md` §6

### Session 4 tests to build with the action layer
- Source: Claude Sonnet 5.5, 2026-10-29
- State: Session 4 decided; see `docs/sessions/04-action-verification.md`.
- Remaining: fault-injection test (refused digs, dropped acks, stale inventories; false `confirmed`
  must be 0); take-over at every phase of bridging, pillaring, eating, chest and bucket use
  (zero fall damage, zero stuck keys, under 50 ms); measure settle windows per action kind.
- Pointers: `docs/research/survey/01-minecraft-agents-2026.md` idea 3, `survey/03-reactive-control-arbitration.md` idea 3

### Session 5 measurements
- Source: Claude Sonnet 5.5, 2026-10-29
- State: Session 5 decided; see `docs/sessions/05-skills.md`.
- Remaining: 20 obtain/ensure skills in the fake world with undo-perturbations at step boundaries
  (0 duplicate placements, 0 wasted crafts, derived guards equal hand-written ones); check the
  format stays readable at 20 to 30 rules; measure effect-declaration accuracy for random drops and
  tool-dependent yields; test 50 skill tasks with and without composition templates on a frontier
  and a cheaper model (adopt templates if the cheaper model gains 20 points or more).
- Pointers: `docs/research/survey/03-reactive-control-arbitration.md` ideas 1, 2, 7

### Session 8 measurements
- Source: Claude Sonnet 5.5, 2026-10-29
- State: Session 8 decided; see `docs/sessions/08-jev-llm-brain.md`.
- Remaining: p50/p95/p99 latency of Jev and the candidate LLMs from the user's PC; does
  cancelling a call stop billing; do the APIs stream structured JSON while reasoning; is Jev
  calibrated with 5 to 80 options and look-alike skills; latency sweeps (Jev +0/150/300/600 ms,
  LLM +0/2/5/10/20 s on 10 seeds) with a paused and a slowed baseline through the logged harness.
- Pointers: `docs/research/decision-models.md`, `docs/research/survey/02-fast-slow-architectures.md`

### Session 7 tests and numbers
- Source: Claude Sonnet 5.5, 2026-10-29
- State: Session 7 decided; see `docs/sessions/07-reflexes-arbiter.md`.
- Remaining: build the reflex coverage matrix with refereed fixtures; arbiter tests (no A-B-A
  within 5 s, preemption within one tick, no starvation death in 30-minute commands); find `k` and
  the hunger and sleep thresholds by measurement; write Nether and End reflexes before their rungs.
- Pointers: `docs/research/survey/01-minecraft-agents-2026.md` idea 2, `survey/03-reactive-control-arbitration.md` ideas 4 and 5

### Session 6 spike and numbers
- Source: Claude Sonnet 5.5, 2026-10-29
- State: Session 6 decided; see `docs/sessions/06-movement.md`.
- Remaining: spike to see whether Azalea's `astar`, `moves` and `costs` can be lifted out of its
  Bevy ECS and client types; tune costs, maximum drop, progress limits and N tries by measurement;
  test that replans looping over the same cells are detected.
- Pointers: `docs/research/systems-review.md` §3.3 and the Baritone section

### Session 11 gate tests and learning test
- Source: Claude Sonnet 5.5, 2026-10-29
- State: Session 11 decided; see `docs/sessions/11-skill-learning.md`.
- Remaining: feed the gate 50 mutants (no-op, wrong count, side effect, protected-block write) and
  50 good refactors (0 mutants admitted, at most 10% of good refactors rejected); red-team test
  (20 broken skills, 0 bypasses incl. chat injection); measure fixture restore time (above 30 s
  means a nightly batch); rollback test (injected degraded version rolled back within 10 uses, no
  false rollbacks across 20 healthy versions); pre-registered learning test at equal compute.
- Pointers: `docs/research/survey/04-skill-library-learning.md` ideas 1 to 6

### Session 10 curriculum comparison
- Source: Claude Sonnet 5.5, 2026-10-29
- State: Session 10 decided; see `docs/sessions/10-autonomy.md`. The user chose a free curriculum
  against the recommendation (ladder spine).
- Remaining: add an arm "ladder spine with code-listed options" next to the free curriculum in the
  equal-compute learning test; check how often free goals end up `unverifiable`; log goals that keep
  Praxion away from the next rung.
- Pointers: `docs/sessions/11-skill-learning.md` (learning test)

### Session 12 checks before building memory and RAG
- Source: Claude Sonnet 5.5, 2026-10-29
- State: Session 12 decided; see `docs/sessions/12-memory-rag.md`. The user chose a wiki RAG against
  the recommendation.
- Remaining: check whether the Minecraft Wiki offers a dump and what its terms allow before
  building the index (the licence question is closed: no commercial purpose, index stays local);
  memory test suite on a scripted world; retrieval test with real questions to confirm the
  embedding model; ask the user what Mnema is, to confirm the separation.
- Pointers: `docs/research/memory-review/critic-simplicity.md` §7, `docs/research/embeddings.md`

### Architecture open points after draft 2
- Source: Claude Opus 5.5, 2026-10-29
- State: Draft 2 accepted in full by the user (decisions.md); review round 1 and the draft-2 review closed.
- Remaining: still [O]: transparent-material table (decided at first code); Mojang EULA and Usage
  Guidelines check before any release (2026-10-31: the official page timed out; third-party summaries
  say only that mods must be free and show no AI-agent rule; read minecraft.net/usage-guidelines
  before release); log the round-1 minor IDs (synthesis.md "Minor and dropped").
  Closed 2026-10-31: lesson trigger, interface technology, commercial parts, wiki RAG licence, `spikes/`.
- Pointers: `docs/architecture.md` §11 and Appendix A

### Later ideas from review round 1
- Source: Claude Opus 5.5, 2026-10-29
- State: proposals only, not decided.
- Remaining: shadow mode while the user plays (check privacy of recording the user's input);
  death-to-fixture factory with counterfactual replay; hidden rotating hold-out fixtures; a
  player-bound fair-perception benchmark. Revisit after Demo-1.
- Pointers: `docs/reviews/2026-10-29-arch-r1/synthesis.md` "Ideas worth adopting"

### Finish the session 2.7 hands experiments (gate for Demo-1 code)
- Source: Claude Opus 5.5, 2026-11-01
- State: E1, E2 (with Azalea patches), E3, E5, E6, E7, E13 passed; E4, E9 measured; all proposals decided
  (`docs/decisions.md` 2026-11-01). Demo-0 is unblocked. Results: `docs/sessions/02-body-connection.md`
  "2.7 Windows runs". Setup: `python setup.py`.
- Remaining (before any Demo-1 code): E8 (needs a mouse-button command in `spikes/hands` and a live decode
  tap), E10 (menu clicks at GUI scales 1-4), E11 (window modes; use a separate `--gameDir` so the user's
  `options.txt` is untouched), E12 (needs the user's physical key presses), E15, E16; E5 pointer-speed
  matrix (the user changes the system setting). `spikes/hands` review leftovers: finding #1 (key slot
  published before its scancode; guarded by the gate except on a same-thread panic) and #6-8 (E12
  timestamps taken at the wrong points) must be fixed before E12.
- Pointers: `spikes/README.md`, `SETUP.md`, `logs/briefs/` (worker briefs, ignored)

### Body restart in the middle of a game connection (before Demo-1)
- Source: Claude Opus 5.5, 2026-11-01
- State: the forwarder sends a gap to a tee subscriber that joins mid-connection; the decoder then
  marks that connection's world view invalid. In Demo-0 the headless client simply reconnects.
- Remaining: Demo-1 plays the user's real client, which cannot be asked to reconnect. Options: the
  forwarder keeps a bounded replay of each connection's login and configuration phase plus the
  latest chunk data (needs parsing in the forwarder, against the decision), or the body asks the user
  to rejoin, or the decoder rebuilds from later full-chunk packets. Decide before Demo-1 code.
- Pointers: `body/crates/tee/src/lib.rs`, docs/decisions.md 2026-11-01 "Tee stream format"

## Done

- Choose a license: Apache-2.0 for the open core, 2026-10-29 (session 15). Open: which parts are commercial.
