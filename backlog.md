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

### End-to-end skills from a free spawn
- Source: OpenCode DeepSeek V4.1 Flash, 2026-11-02
- State: Step 9c added the frontier `explore` primitive, bounded child failures, drop pickup, and
  fixed the block-receipt false negative (a place into a replaceable clicked block, and a dig whose
  block update arrived before registration). Stocked runs now pass 6/6 with every receipt confirmed
  and zero failures (186-195 ticks). Fresh-spawn `get_wooden_pickaxe` passed on seeds 101 (1113
  ticks), 202 (1704) and 303 (1292), all referee-confirmed. The far-logs desert run (seed 202,
  teleported to 2000/-3056, no log within 40 by referee queries) found a tree and finished in 496
  ticks. Fresh-spawn `get_stone_pickaxe` from nothing did not reach the pickaxe within 600 s on any
  seed; a 1200 s 101 attempt was still churning planks/logs at tick 7136 when stopped.
- Remaining: (1) make the fresh stone chain reliable: keep a spare table or enough planks after the
  wooden pickaxe (gathering nine planks would leave four), return to the placed table instead of
  crafting another, and approach cave stone from an exposed face; (2) `explore` forgets its explored
  set between calls, so a fruitless 1200-tick path can repeat (the child-failure bound stops the
  livelock, but the search is not cumulative); a per-run explored set or a rotating start direction
  would fix it; (3) rerun the fresh stone chain with a longer cap once (1) lands.
- Pointers: `logs/s9c/e2e-stocked-final-stocked.json`, `logs/s9c/e2e-fresh-final.json`,
  `logs/s9c/e2e-202-303.json`, `runs/s9c-*`, `notes.md` 2026-11-02.

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

### Step 9 follow-ups (before step 10)
- Source: Claude Opus 5.5, 2026-11-01 (step 9 worker report)
- State: reflexes and arbiter written, 47 tests and a 72-cell coverage matrix pass on a mirror build; the real
  workspace did not build at that moment because of step 8b's in-progress gamedata and headless changes.
  The independent review (`logs/demo0/t9-reflexes-arbiter.review.md`) found 2 major and 6 minor items; all
  eight are fixed (2026-11-01): unknown blocks stay unknown in the reflex layer, the own entity data feeds
  air and burning, plus the pitch sign, the zero tick settings, the nearest Enderman, the push-before-start
  order, the single-step lava/unknown refusal and the arrow strafe side.
- Remaining: (1) verify and commit step 9 in the real workspace after 8b; (2) done: the player's own entity
  data (shared flags: burning; air supply) is parsed in `praxion-world` (`own_air`, `own_burning`), exposed
  through perception (`air`, `burning`, HUD facts) and the drowning and fire reflexes read them, keeping the
  simulated air only as the fallback while the fact is unknown; (3) the melee reflex's `Attack.weapon` must
  reach the actuator through `ControlFrame.select_slot` once 8b adds it; (4) done: independent review of
  step 9, findings fixed.
- Pointers: `logs/demo0/s9-reflexes-arbiter.answer.txt`, `logs/demo0/t9-reflexes-arbiter.review.md`,
  `body/crates/reflexes`, `body/crates/arbiter`

### oc-run.sh: unique session title per run
- Source: Claude Opus 5.5, 2026-11-01 (review rv1 by Iceman, finding "title-only session match")
- State: `stop_session` finds the session by title = brief name; two runs of the same brief at once could
  export and delete the other run's session.
- Remaining: pass `--title "$name-$$-$start"` and match that exact title. Edit only when no oc-run.sh is
  running (notes.md 2026-11-01).
- Pointers: `.claude/skills/opencode-worker/scripts/oc-run.sh`

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
