# Notes

Dump space for the user's ideas and for agents' notes, observations and proposals.
Newest first. Prefix each entry with a date and author. Nothing here is a decision
until it is recorded as one. Promote decisions out of this file when they are made.

---

## 2026-11-08, Claude Opus 5.5: autonomous run 3: six rungs, then a self-written skill
- Brain loop + body port, fresh seed 202, no help: proposer -> Jev/code routing -> body played wood, crafting
  table, wooden pickaxe, 8 cobblestone, stone pickaxe, stone sword (`runs/test-body-7ad57f43c2664f0f86d620b722cbf912`).
- At the furnace rung no skill existed: route `write`; `gpt-6-luna` wrote `get_furnace` on the first attempt and
  admission passed it 4/4 (seeds 101, 202, 303 + held-out 404); it went to `local/skills/active/`. Session cost
  $0.035 in all.
- The live body then refused `get_furnace` (`unknown_skill`): it loads skills only at startup. Hot load of the active
  skills directory is the next body piece.
- Runs 1-3 fixed on the way: Jev routing (exact names in code, rules in the state), cumulative ladder, tags over
  variants, live port tick, unverifiable conditions not sent, count args, anti-spin backoff, per-run memory, the wet
  stone rule, mine's search radius.

## 2026-11-08, user: building, persona, combat, farms (wishes, not yet decided)
- The bot must build shelters, and not the same shelter every time: a design that fits the site and the
  moment, and beautiful when it wants to be. It should have a persona.
- It must fight mobs, go wherever an item's mob lives and hunt it, and even build a mob farm.
- Claude's proposal (not decided): a build layer (LLM designs a voxel blueprint for the site from the persona,
  terrain and inventory; code validates supports, materials and reach; a `build` primitive executes it
  support-first with scaffolding), a persona profile fed to the proposer and the designer, a `hunt(mob)` core
  skill plus combat tactics, then farms as blueprints with mechanics from the wiki RAG. After the Demo-2 loop.

## 2026-11-08, Claude Opus 5.5: first live `learn get_furnace`
- The whole slice ran end to end: Jev routed "have a furnace" to write (every known skill ~0.02), the writer
  produced four candidates (2x `gpt-6-luna`, 2x escalated `gpt-6.1-terra`), all passed `check-skills`, and the
  third (terra) made a furnace on seed 101 in 5140 ticks. Total cost $0.009 (OpenAI) + $0.00006 (Jev).
- Admission still rejected it: every practice failure was a body failure (`explore(stone) not_found`,
  `mine(stone) no_block`), which the writer cannot fix (architecture §6.2). Admission now separates body from
  skill failures (spare seeds, `local/brain/body_failures.jsonl`).
- Body gap exposed: in flat terrain stone is under the dirt and the body has no way down. A person digs a
  staircase. Next body capability. Report: `local/brain/admission-get_furnace-20261108T094429474802Z.json`.

## 2026-11-08, Claude Opus 5.5: lively gaze, far sight, explore
- C3 lively gaze (1026235), C4 far cue (6a9a1da, 799a476) and explore seek-while-walking plus a
  look-around turn (ca9bf47) landed. Live run 2: 18/18 receipts, pitch now varies (123 distinct values).
- `nearest_known` at radius 64 was 148 ms worst case (O(r^4) shell scan); the section-ordered
  `KnownMap::nearest_seen` brings it to 15 ms.
- Explore still fails about 2 of 5 runs on seed 303: a hill hides the tree. Proposal: score frontiers by
  height or unseen sky area so the bot climbs for a view (backlog).
- Never `git worktree remove --force` a worktree that holds a `third_party` junction: it empties the
  real `third_party` (recovered with `python setup.py`).

## 2026-11-07, Claude Opus 5.5: movement style and worker notes
- Gaze (15e512d), walking (00ea434) and clippy/duplicates (b75d50f) landed. Physics following tests got
  no slower (terrain 2210 -> 2190 ticks). The real test of "less robotic" is a live `watch` run, not yet done.
- Proposal: next movement pieces in order: diagonal string-pulling (needs a fix for narrowing corridors),
  idle gaze and pitch noise (research §4.6), then measure against human traces (research §6).
- Worker hygiene: a DeepSeek worker overwrote and deleted an untracked `final_test.log` at the repo root
  despite "touch only body/". Briefs now say "no log files anywhere".

## 2026-11-07 - OpenCode DeepSeek V4.1 Flash - s9d: per-rule failure counts and leaving a peak

- **Per-rule failure counts (decision 2026-11-02) are in.** A frame keeps a `BTreeMap<rule, count>`;
  a rule's own success clears only its entry, another rule firing leaves it alone, and the third
  failure of any one rule fails the frame with that kind (`interp.rs:43`, `:657`, `:718`). The A-B-A
  test (`tests.rs:631`) is the live shape from `runs/s9d-f11-3-seed303` seq 83-125.
- **The peak failure was a drop, not the map.** Seed 303's run ended in a one-cell pit after
  `explore` planned an eight-block drop (the WIP had raised `max_drop` to 8 to leave the plateau).
  It now uses the planner's safe three-block drop (`runner.rs:1604`), and the follower releases
  forward once airborne instead of carrying the fall into the next ledge
  (`follow.rs:129`, test `:284`). A peak is left along its slopes.
- **Nearest-first exploration circles the sight ring.** Seed 202 is plains with its forest 160
  blocks away (`locate biome`: `[84,65,144]`, evidence `logs/s9d/biomeprobe/`); nearest-first walked
  the ring at the 80-block sight edge for 4,000 ticks. The first eight goals of a call stay
  nearest-first, then farther-from-origin frontiers win (`runner.rs:1214`, `:1789`, test `:2959`).
  Seed 202 then finished in 2,208-2,685 ticks in the probes.
- **Still open:** the `get_stone_pickaxe` video runs show the same pit problem from spawns the
  wooden gate does not sample; known-map lag after a place; the runner is still a script, not a
  `-m server` harness test.

## 2026-11-03 - Claude Opus 5.5 - macOS session: setup, test parity, first stone from a fresh spawn

- **macOS works for body work.** `setup.py` finished without downloads by seeding
  `third_party/azalea` from the old `spikes/round0/azalea` clone and the server jar from
  `spikes/round0/dl/server.jar` (sha1 matches the manifest), with `--java spikes/round0/jdk/...`. Only
  the rustup `llvm-tools` component was fetched. The spike proxy linked `kernel32` unconditionally
  (fixed). The old round-0 Mac downloads under `spikes/round0/` (`jdk/`, `dl/`, `azalea/`, logs) show
  as untracked: `.gitignore` does not cover them.
- **Release and debug test runs differ.** Azalea's leftover-data check is `#[cfg(debug_assertions)]`;
  in release (the profile the body runs) the decoder's own `unread_bytes` backstop names the desync.
  The Windows "609 passed" count was debug. Run both when a test touches Azalea behaviour.
- **Only the innermost frame selects.** A rule in a parent skill is not evaluated while a child runs;
  the parent re-evaluates when the child ends. A rule that must fire "before the body leaves" belongs
  in the skill that decides to leave. The independent review caught this before a live run did.
- **The headless client does not predict inventory clicks.** A swap shows only after the server's slot
  update, so any primitive that re-sends a swap every tick undoes it. `HandSelect` sends once and
  waits; `equip` already did.
- **Fresh-spawn seed 303 reached 3 cobblestone with a carried table**, the first fresh run to mine stone
  with a tool. All three seeds now fail on other steps: place target selection (202, 303) and the
  explore pit (101). See backlog "End-to-end skills from a free spawn".
- OpenCode v2.1.2 on this Mac runs `opencode-go/deepseek-v4.1-flash`; one refactor and one review ran
  through `oc-run.sh` cleanly (exit 0, no out-of-scope edits).

## 2026-11-02 - OpenCode DeepSeek V4.1 Flash - s9c: explore, child failures, pickup and the place false negative

- **Bounded child failures**: a child's `failed(kind)` now increments the calling rule's failure
  streak; below three the parent waits `Waiting("retry")` instead of failing `no_rule`, so a parent
  with no other true rule retries next tick. `done` and a different rule firing reset the streak.
  Tests: `three_consecutive_child_failures_fail_the_parent`, the success-reset and different-rule-reset
  tests, and `a_child_no_rule_failure_counts_towards_the_parent_streak`.
- **Place false negative, root cause (captured tee, stocked 202 wooden)**: place intent 5 registered on
  tick 154, actuator hit `(-13,65,11)` face North; the driver watched the adjacent `(-13,65,10)`. The
  decoded capture has the `crafting_table` at `(-13,65,11)` (the clicked cell) and air at the adjacent
  cell: the clicked block was replaceable (tall grass), so the server placed *into* it. The receipt
  watched the wrong cell and settled `unconfirmed(missing_block)`. Fix: `place_target` skips
  replaceable supports (`place_support`), and `Place` re-plans if the target becomes the body's feet
  or head cell after a step (the 303 stone failure: the target was the head cell and the server
  refused). After the fix all six stocked runs have zero `missing_block`.
- **Dig false negative, same before-registration shape (captured tee, fresh 101 stone)**: dig intent 15
  registered on 2642 with start tick 2641, target `(-352,74,-144)`, pre-click stone, the world already
  air; the ack (seq 217) arrived on 2643. The old receipt saw only the ack and settled
  `unconfirmed(missing_block)`, while the decoded final block is air. Fix: the driver passes the
  observed block on dig intents and `Receipt::new` presets `block_air` from non-air pre-click + air
  observed. Test: `a_dig_update_before_registration_still_confirms_with_its_ack`. Fault injection stays
  at 0 false confirmed.
- **Explore radius readings**: the skills guard on `not near(T, 5)`, the radius of the `mine` search,
  because `near(T, 32)` alone let mine fail `no_block` at 20 blocks. `explore` searches 16 blocks for a
  known target (32 cost too much tick time), approaches a known target directly, and only falls back
  to a frontier. The frontier scan prefers the player's level (a cave dive was the first failure),
  rejects cells behind unknown cells, avoids explored cells, drops world-change moves, and blacklists
  the cell where a follower stalled. First version scanned 32 blocks every tick and dropped the live
  loop to ~3 TPS; the wide scan now runs only when a new plan is needed.
- **Pickup**: after a confirmed break the runner walks to the item entity for at most 60 ticks; the
  mine receipt remains the block-break receipt, no inventory receipt is invented.
- **Open proposal**: `get_wooden_pickaxe` leaves three planks, so the later stone skill often explores
  for one more log to craft its table. Gathering nine planks (three logs) would leave a spare table's
  worth; not done because it changes the first-night curriculum beyond this step.
- **Live, final binary**: the stocked table is 6/6 with 7 receipts confirmed each and zero failures
  (186-195 ticks). Fresh spawn `get_wooden_pickaxe`: 101 1113 ticks, 202 1704, 303 1292, all
  referee-confirmed and zero failures (each on the explore build; 303 from the earlier batch, the
  later 303 attempt found a tree at tick ~4000 but the 600 s cap cut the craft). The far-logs desert
  check (seed 202, teleported to a desert at 2000/-3056 with no log within 40 by referee queries)
  found a tree and finished `get_wood` in 496 ticks. Fresh-spawn `get_stone_pickaxe` from nothing did
  not reach the pickaxe within 600 s; the long 1200 s attempt still gathers wood at tick ~4700. The
  stocked stone 3/3 pass, so the remaining gap is curriculum/exploration efficiency, not a primitive
  or receipt failure.



- `praxion-live` in skill mode finished the whole skill on the first primitive outcome, twice: the
  `runner.outcome()` precheck and the post-tick `result.outcome` check. The interpreter never advanced.
  Both finishes are now gated to `LiveMode::Primitive`; the outcome goes to the bridge.
- The interpreter's flight deadline came from the rules registry and was shorter than the primitives'
  own waits: place 20 vs a 40-tick receipt, craft 20 vs 140 take, eat 10 vs 32+ ticks, attack 20 vs 114
  in the live table, equip 5. The interpreter retried and dropped actions. Defaults now cover the
  internal bounds (mine 180, place 80, craft 300, eat 180, attack 420, equip 80, observe 100), and a
  timeout cancels the runner (`Runner::cancel`) instead of letting it run beside its retry.
- `move_to` lost a chosen block goal when a replan happened out of sight (pool: `no_target`) and a cell
  two blocks below the marker counted as "near" (303 stairs stopped under the plateau). The goal is
  cached and the radius is 1.5.
- The planner could not climb out of a flush 2-deep pool: its climb branch required water beside the
  body instead of the land top one block up (`planner/src/moves.rs`). Fixed with a unit test.
- Receipts did not understand tag items (`craft(#planks)` never confirmed) and a place whose block
  update arrived before registration expired `missing_block`. Both fixed with tests; the 10,000-action
  fault injection still reports 0 false confirmed.
- Free-spawn skills still fail: `mine(#logs)` searches 5 blocks and no skill explores; the interpreter
  retries the failed child forever (900 ticks/45 s, 29 no_block, outcome null). In the backlog.

---

## 2026-11-01 · Claude Opus 5.5 · Free models on Demo-0: good reviewers, weak long coders

- Beam Free: a strong source survey (Azalea headless, every claim with file:line) and a good
  independent tester (9 new forwarder tests, 4 real findings, a live status ping). But on the harness
  (a long coding job) it timed out after 60 min with no report.
- Iceman Contributor Free: the server survey left easy checks "unverified"; the headless login attempt
  timed out after 80 min with code that does not compile (pulled azalea-auth and bevy). Reviews only.
- DeepSeek v4.1 Flash finished every coding job it got (manifest, move, forwarder, decoder, harness,
  Azalea patch). Default: DeepSeek writes, Beam tests and reviews, Iceman takes small reviews.
- DeepSeek hand-writes what a mature crate already does when a brief does not forbid it: a 590-line zlib
  inflater and a ZIP reader in two days. Every coding brief now says: use mature crates (flate2, zip,
  serde_json); never hand-write a parser or codec.
- An independent test that "flaked once" failed 8 of 15 in a loop. Always loop a reported flake.
  Root cause: on Windows a socket handle duplicated with `TcpStream::try_clone` can stay "not
  connected" (WSAENOTCONN) for `shutdown` while reads and writes work, so the half-close FIN is never
  sent and the connection hangs. The forwarder now shares one handle per socket through `Arc` and tears
  the pair down if a half-close fails. Watch for `try_clone` + `shutdown` in any other socket code.

---

## 2026-11-01 · Claude Opus 5.5 · Never let a worker edit a script that running workers execute

Three parallel `oc-run.sh` runs died with exit 2 ("syntax error near `)'") when a fourth worker edited
`oc-run.sh`: bash reads a script lazily, so a running instance picks up the half-changed file. The
workers themselves finished (the service keeps going); only the wrapper crashed. Edit shared tooling
only when no run uses it, or copy the script per run.

---

## 2026-10-31 · Claude Opus 5.5 · Lessons from the first Windows 2.7 runs

- Free workers claim "all fixed" when they are not: a hands fix left a ring buffer, sliced waits and the
  watchdog pipe defined but never called (`cargo build` dead-code warnings gave it away). Ask for a
  finding -> file:line -> "on the real path" table, and require zero warnings.
- Worker self-tests can test a copy of the code (the first proxy self-test did), and a passing self-test
  missed a Windows-only bug (accepted sockets inherit the listener's non-blocking mode). Only the live run
  against the real client caught it. Always validate spikes against the real game.
- Periodic experiment timing can phase-lock to the game tick (450 ms gaps = 9 ticks) and hide variance.
  Use gaps that are not multiples of 50 ms.
- Workers also rewrote `.gitignore` and recreated a deleted crate-local config; check `git status` and
  ignored-file rules after every batch.

---

## 2026-10-29 · Claude Opus 5.5 · Lessons from the first multi-model review round

- OpenCode workers on free models ignore "read-only" in the brief: they wrote scratch files into
  the repo root and one used Playwright MCP. Inspect `git status` after every batch. Proposal:
  a read-only OpenCode agent (`.opencode/agents/reviewer.md` with edit denied) for review work.
- OpenCode `websearch` failed in every run (19 errors); `webfetch` worked. Web research belongs to
  the Claude subagents until this is fixed.
- Free verifiers (Iceman, Beam) refuted 0 of 45 clusters and rated most as "partly". They are good
  for severity down-ranking and for finding where the docs already answer a finding, but they are
  no filter. Keep the Opus adjudicator.
- Beam Free merged 204 findings with full ID coverage; Iceman missed 65. Prefer Beam for long-context merges.

---

## 2026-10-29 · User, answered by Claude Sonnet 5.5 · Reinforcement learning for some skills (idea, not a decision)

The user asked whether to train an RL model for some jobs. Not part of the first demo (session 8).
Candidates, in order of fit: (1) combat micro, bridging, water-bucket clutch: narrow, 20 Hz,
could replace hand-written Rust primitives, which stay swappable; (2) imitation of the user's own
motor style for naturalness (survey/01 idea 1; imitation, not RL); (3) a high-level decision
policy: not realistic for this project. Obstacles: Minecraft is slow to train on, "zero cheats"
limits shortcuts, RL policies tend to look robotic, and the user must understand every part.
Revisit after baseline measurements, for the primitive where the hand-written version falls short.
Unchecked: tick acceleration with `/tick rate`, existing RL environments (MineRL, MineDojo,
Craftium), VPT and PLAICraft data licenses.

---

## 2026-10-21 · User (recorded by Claude Opus 5.5) · Goals for Praxion

In the user's words, translated:
1. **Technical plan:** Jev with LLMs behind it. A better proposal is welcome.
2. **Feel:** the agent must feel like a natural player.
3. **Aim:** an agent system that thinks for itself and plays on its own, in two ways:
   - **Companion:** joins the user's world as a separate player.
   - **Player-bound:** the user hands over their own screen, i.e. their own character, and the agent plays it.
4. **Commands:** the player can give direct commands ("collect ore" and the like) and the agent must accomplish them.

## 2026-10-21 · Claude Opus 5.5 · Implications of the goals (proposals, not decisions)

- **Two modes mean two bodies.** Companion mode works through a separate protocol client (what
  mineflayer does). Player-bound mode needs control from inside the user's real client: a client
  mod (as Baritone and altoclef do on Fabric) or screen in, keyboard/mouse out. Mineflayer cannot
  serve the second mode at all. A body port, so one brain plugs into either body, belongs in the
  architecture from day one.
- **"Natural player" decides perception.** Reading blocks through walls breaks the feel. This
  points to a fair-play perception policy with provenance tags (Craftonomous). For movement,
  mineflayer-pathfinder's `human.js` calibration and look quantization become product requirements.
  No operator commands follows from the same goal.
- **"Must accomplish commands" means reliability and verification.** Frequent commands (collect,
  fetch, mine, craft) need deterministic, verified skills rather than LLM improvisation; Odyssey's
  tech-tree solver and altoclef's `ResourceTask` are direct precedents. How to read an ambiguous
  command is its own decision.
- **Playing alone also needs goal generation, long-term memory and self-assessment.** Voyager's
  curriculum does this but only proposes goals its inventory-based critic can check.
- **Commands, the agent's own plan and reflexes must meet in one arbiter.** In altoclef the user
  task sits at fixed priority 50, below survival reflexes.
- **Jev fits the middle layer, not the tick loop.** A hosted round trip of 70–500 ms cannot serve
  50 ms reflexes (lava, falling, low health); those stay in local code. Jev suits second-scale
  tactical judgements: is this mob a threat, flee or fight, is a chat message a command and which
  skill does it map to, does this need the big LLM.

## 2026-10-21 · Claude Opus 5.5 · Repo bootstrap

- `AGENTS.md` is the single source of truth; `CLAUDE.md` only imports it (`@AGENTS.md`).
  Reason: OpenCode v2 reads only `AGENTS.md` and Claude Code reads `CLAUDE.md`, so an import keeps
  them in sync by construction. A copy-and-sync scheme would drift.
- OpenCode v2.1.2 is installed. The free `minimax-m3.1-flash-free` works through both `opencode/` and
  `opencode-go/`. It answered a small bug-spotting task correctly in 5 s at cost 0.
- The global `opencode-cli` skill (`~/.agents/skills/opencode-cli`) was written for OpenCode v1 and
  Yele's old model ladder, so it is out of date for v2. For example, v2 has a shared background service
  and permission rules are a list of `{action, resource, effect}`.
  The project skill `opencode-worker` replaces it here. The global skill should get a v2 update later.
- `opencode.json` deny rules hold under `--auto`. One gap: shell commands that use `~` paths
  skip the external-directory check.
- Proposal: have the first review round right after the architecture draft, using a model other than the drafter's.
