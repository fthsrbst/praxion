# Notes

Dump space for the user's ideas and for agents' notes, observations and proposals.
Newest first. Prefix each entry with a date and author. Nothing here is a decision
until it is recorded as one. Promote decisions out of this file when they are made.

---

## 2026-11-13, Claude Fable 5.2: demo night orchestration notes
- Measured on 23 recorded runs before the change: proposal call p50 7.7 s (`gpt-6-luna`), 108 600 ticks in
  `nothing_to_do` back-off (rate limit 33 times). After: one planner call per 1-5 goals; `gpt-6.1-terra` answered one
  briefing in 4.6 s with reasoning effort `none` and 9.6 s with `low` (single measurements, not a distribution).
- Four Rust workers with their own `CARGO_TARGET_DIR` filled the disk (ENOSPC) and killed every worker and the
  Bash tool. From then on all Rust workers shared `body/target` and waited on the cargo lock. Rule for briefs:
  never a private target directory on this machine (19 GB each); serialise builds instead.
- Workers that died mid-edit were resumed with a short preface ("partial edits are in the tree, read the diff,
  finish") in front of the unchanged brief; all five finished correctly that way.
- GLM 5.3 Flash reviews again found real defects every time (10 in the brain, 3 in the body), none invented.
- A Jev score just under a threshold threw away a correct planner choice (`hunt` 0.49 against 0.5); the planner's
  named skill now needs only to clear the write threshold.
- Body behaviours that read as "not human" in live runs, all from skills and primitives rather than the model:
  three 1200-tick surface explores for stone, mining the same block under water in a drowning loop, mining at a
  log six blocks overhead from the bottom of a dig, a dropped crafting table never picked up, nine minutes sealed
  in a shelter. The model layer was the smaller part of "it does not feel smart".
- The promo video is built from one HTML composition stepped frame by frame in headless Chrome
  (`video/promo/`): `python3 build.py render tr|en`, then `mux`. Voice-over and the ambient loop come from
  ElevenLabs (the music API needs a paid plan; the sound-generation endpoint works on the free one).

## 2026-11-12, Claude Fable 5.2: orchestration notes (metrics, run 10, S1, S2)
- Workers this session: DeepSeek V4.1 Flash wrote `metrics.py` (+ fixes) and S2 shadow mode (+ fixes);
  GLM 5.3 Flash reviewed the mouse probe, the metrics and S2. Every review found real defects
  (z-order hole in the cursor check, a restart losing half a run, no death handling in shadow mode).
  One DeepSeek run ended without the `FINAL_REPORT` marker (exit 12) although the work was complete:
  briefs now say "your last message MUST end with a line that starts with the exact word".
- `oc-reap.sh` after every worker exit stopped a live session each time: finished sessions do linger.
- The practice room competes with the live run for CPU: a client launched during run 10's admission
  needed more than 45 s to join. Admission practice time (278-669 s per seed) is the main cost of
  skill writing, not the model (3 s).
- Proposal: a scenario runner (`python -m praxion_harness.scenarios`) that plays a fixed list
  (seed, minutes, peaceful/survival, optional operator task) one after another and writes the
  compare table; first set: seeds 1010 (done), a forest spawn, a plains spawn, a pit spawn (808),
  a night start, and a chat task run. Not built; needs the user's yes on the list.
- Proposal: an LLM judge (GLM) that reads `metrics.json` plus the goal timeline and marks each
  goal sensible / wasteful / silly with a reason; cheap, runs after each scenario. Not built.

## 2026-11-12, Claude Fable 5.2: wiki RAG source check, mouse/camera hypotheses
- Wiki RAG is not built: no index code in the repo, nothing for praxion in the hub RAG.
- minecraft.wiki facts read today: text is CC BY-NC-SA 3.0 ("additional terms may apply", full
  terms on the Weird Gloop meta Licensing page, not read). `robots.txt` for `User-agent: *`
  disallows `/*api.php`, `/w/Special:` (so Special:Export), `?action=` and `rest_v1`; ordinary
  article pages under `/w/` are not disallowed; no crawl-delay; a sitemap is published; named AI
  crawlers (GPTBot, ClaudeBot, CCBot and others) are blocked site-wide. No official dump was found
  (two searches; unknown, not proven absent).
- Proposal (not decided): do not use the API or Special:Export. Exact game data (recipes, loot,
  tags) comes from the game jar through `gamedata`, not from the wiki. For prose knowledge
  (mechanics, mob behaviour, farms) either ask the wiki admins for a dump or permission, or fetch a
  small fixed list of article pages (hundreds, not the whole site) slowly with an identifying
  user agent, index locally and never distribute. The user decides.
- Mouse on macOS (hypotheses, untested): the failed click was PID-targeted (`postToPid`). Mouse
  events posted to a PID skip the window server's routing, and a client in relative-mouse mode
  reads deltas, not positions. Next spike step: post at the HID tap (`CGEvent.post(tap:
  .cghidEventTap)`) while Minecraft is frontmost, for buttons and for camera (`mouseMoved` with the
  delta fields set), and tag own events (`eventSourceUserData`) so the listen tap can tell them from
  the operator's physical input. Cost: a HID-level post is global, so the focus check becomes a
  hard precondition. Keyboard-only fallback covers attack/use (rebound keys) but not the camera.

## 2026-11-12, Codex: dashboard same-player route
- The approved UI selects "Bot controls my player" beside server start. Joining leaves the user
  in control; "Give Praxion control" is separate. Keep the server up after a failed join or a
  release, and allow an explicit retry without spawning a headless login.
- The existing dashboard cannot gain this behavior by relabeling Start bot:
  `body/crates/body/src/main.rs` calls `praxion_headless::Session::connect_with`, which creates
  a new Minecraft login and owns the tick loop. The real-client path needs to feed the existing
  decision loop from the one joined client and apply its actions through native input.
- On macOS the product forwarder tee was attached before the vanilla client joined. The final
  keyboard spike walked 6.472 blocks with only that client connected. Offline decode of the live
  capture matched server position and inventory (453 packets, 0 failures, 0 desync). This verifies
  the traffic path, not the live tick loop or reconnect.
- The fixture placed a stone block under the crosshair in Creative. PID-targeted Quartz mouse
  down/up did not change the block, including with the main window ID and center. With attack
  rebound to F in an isolated client profile, the keyboard spike broke the block and the
  decoder saw a block acknowledgment. The cause of the mouse result is still unknown.

## 2026-11-12, Codex: macOS same-player input
- PID-targeted Quartz keyboard events reached vanilla 26.3/SDL Cocoa with just the local client
  joined. A real Space press stopped a one-key hold and reached the game as a jump. This supports
  testing a macOS input backend, with mouse and menus still unknown.
- A terminal state released W but the original process hung: `RunLoop.main.run()` re-entered with
  installed sources after `CFRunLoopStop`. Switching to `CFRunLoopRun()` returned to cleanup and
  exited in the live timeout and interruption runs.
- The first client launch stalled in the SDL/OpenGL render path; the isolated retry with the existing
  watcher options joined. The test does not isolate the startup cause. Sample:
  `logs/mac-hands-live/client.sample.txt`.

## 2026-11-09, OpenCode (DeepSeek V4.1 Flash): smarter slice 2 review fixes
- The scaffolding set left `praxion-primitives` for `praxion_perception::scaffold`. The review's HIGH finding was
  that `mine`'s target search skipped the body's own scaffold log and the `near` fact did not, so `get_wood` could
  chop nothing, burn its held logs on a column and fail; the facts crate could not see the primitive crate's
  registry. Perception holds it now, and all four call sites ask the same `is_scaffold`.
- A scaffold cell is remembered only when the place receipt confirms it, and forgotten when any sight of it finds it
  empty again (our own dig settled, or something else broke it). Recording it at click time meant a refused click
  hid a real block from every target search for the rest of the process.
- The pillar's call bound was shorter than the worst case its own stages allow (two jumps plus a receipt) and a
  jump that never got a click reported `no_place` as if a placement were refused; the bound is 165 against a
  registry budget of 180 and that case is now `no_click`.
- Remaining design questions (backlog "smarter slice 2 review leftovers"): a failure-kind fact for the climb rule,
  and whether `mine` should walk before it fails `out_of_reach`.

## 2026-11-09, user: voice (for the demo/presentation)
- The bot will talk with us by voice through ElevenLabs: speak its narration, ask questions, and react to the user's
  commands.
- The user will hold a hotkey and speak a command; it is transcribed and goes into the same command path as chat and
  console text (operator-only, control verbs parsed locally first).
- Not designed yet. Builds on the talk slice (Turkish narration, chat/console commands). Session 9 listed "voice channel:
  later"; this makes it a planned feature. Open: TTS/STT providers and cost, latency budget, where audio runs (brain
  machine), privacy of recordings (architecture §7: local only), hotkey on Windows vs macOS, key storage.

## 2026-11-09, Claude Opus 5.5: autonomous run 5 and find_ore
- Run 5, fresh seed 505, commit 0c27a5f: wood -> crafting table -> wooden pickaxe -> cobblestone (x8) -> stone pickaxe
  -> stone sword -> furnace, all done, about 4 minutes; the furnace again by the self-written `get_furnace`.
- Coal: route `write`, `gpt-6-luna` wrote `get_coal` (mine if near, else explore(coal_ore)); practice failed
  `not_found` on every seed -> `inconclusive_body`. Ore is underground; explore only walks the surface.
- Two loop bugs seen live, fixed: (1) while the skill was written, the proposer repeated the learning goal, code
  rejected it as `same_as_last`, escalated to gpt-6.1-terra and idled (c1b7405: exclusion checked before validation,
  prompt asks for a side goal); (2) after the failure it repeated the failed goal (rule missing from the prompt, f1be00e).
- Body: `branch_mine(ore)` + core skills `find_ore(ore)`, `search_surface(block)` (7ab0ef5, Sonnet writer, GLM review,
  DeepSeek fixes). Not yet run live.
- Lesson: a worker's `cargo fmt` reformatted ~105 unrelated files; never put `cargo fmt` in a brief here.

## 2026-11-08, OpenCode (DeepSeek V4.1 Flash): a pop cancels a running goal
- The body's `pop` now cancels the running intent instead of ignoring it: the in-flight primitive is abandoned,
  the command settles `settled outcome=cancelled`, the stack entry is `done` and a suspended goal underneath
  resumes through the same settle path (`close_goal` in `driver.rs`). No wire change; the pop already carried
  the intent id, so the golden frames did not move.
- The brain closes the stopped goal only on that receipt (`_on_receipt` maps `cancelled`/`popped` to
  `interrupted`/`stopped_by_user`), so the console's "Stop goal" is confirmed by `goal_finished`, not optimistically.
- Observation: `_stop_goal` no longer moves the loop to IDLE while the stop is unconfirmed, so a body that ignored
  the pop would hold PLAYING until the goal's `budget_ticks` settle. Every command carries a budget, so this is
  bounded; a stop watchdog is only worth it if a port consumer ever sends a command without a budget.

## 2026-11-08, Claude Opus 5.5: autonomous run 4: the loop closes
- Fresh seed 404, never played before, built from commit 480ed5a (console + hot load), watched by the user live and in
  the web console. Seven rungs with no help: wood, crafting table, wooden pickaxe, cobblestone, stone pickaxe, stone
  sword, and the furnace with `get_furnace`, the skill the brain wrote and admitted itself in run 3 (done in 313 ticks).
  Gap found -> skill written -> admitted on 4 seeds -> used in a new world: the Demo-2 promise, end to end.
- Next rung (coal): routed to write again. Background admission (body keeps playing) and the body cancel land next.

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
