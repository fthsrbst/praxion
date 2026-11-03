# Decisions

Agreed decisions, newest first. Each entry: date, decision, reason, what it rules out.
Proposals and ideas stay in `notes.md` until they are decided here.

## 2026-11-03

All on Claude's recommendation, agreed by the user, except where marked (user).

- **The movement-style layer is part of Demo-1, not earlier (user, 2026-11-03).** The first demo video
  (headless body, first-person spectator) looked robotic: one-step exact aims, constant speed and
  rhythm. That is the movement-style layer's job (session 13), not the brain's: Jev and the LLM decide
  what to do, not how the body moves. The user kept it in Demo-1, with the real client, and it is now
  named in the Demo-1 slice (architecture §1.1). Rules out: building it on the headless body first.
- **The body takes its placed crafting table along before it explores for stone.**
  `get_cobblestone` gains one rule above its `explore(stone)` rule:
  `has(wooden_pickaxe) and not near(stone, 5) and near(crafting_table, 4) and not has(crafting_table)
  -> mine(crafting_table)`. The table drops itself and is placed again by `get_crafting_table_near`
  once the cobblestone is in. The rule lives in `get_cobblestone` because that skill decides to leave
  and only the innermost frame selects a rule each tick; a first draft in `get_stone_pickaxe` could
  never fire (review, 2026-11-03). Reason: the wooden pickaxe leaves three planks,
  one short of a second table, so a fresh-spawn body that had walked to stone explored for one more log
  and never reached the stone pickaxe within 1200 s (backlog, step 9c). Rules out: crafting a spare table
  before leaving (one more log, a table left behind) and walking back to the placed table (long, risky
  return from a cave, bounded by the known-map radius).
- **`mine` selects its own tool.** Before the first attack click, `mine` puts the best item it
  carries into the main hand by the vanilla rules the headless client already ports (an item the
  block drops with beats one it does not, then the faster base destroy speed); a tie keeps the held
  item. A block that drops nothing without a tool the body lacks fails `needs_tool` instead of being
  broken for nothing. Reason: on a fresh spawn the body mined 21 stone by hand and collected no
  cobblestone, because `mine` never changed the held item. Rules out: an explicit `equip` rule in
  every mining skill (repeated, and silently wrong when forgotten).
- **`explore`'s memory belongs to the run, not to the process.** The recent explored cells, the home
  altitude, the explore origin and the rotation counter move from process-wide statics into an
  `ExploreMemory` owned by `Runner`. Behaviour is unchanged. Reason: globals leak between runners
  (parallel tests) and hide the "one process is one run" assumption. Rules out: keeping the statics.

## 2026-11-01 (Demo-0 start)

All on Claude's recommendation.

- **Failure counts are per rule within a frame, reset only by that rule's success (2026-11-02).** The
  first rule ("a different rule firing resets the streak") livelocked live: on seed 303 the parent
  alternated `get_sticks` and `get_planks`, each failing `not_found`, for 2,200 ticks
  (`runs/s9d-f11-3-seed303`, seq 83-125). Now each frame keeps a failure count per rule; a rule's
  success (primitive confirmed or child `done`) resets only its own count; the third failure of any one
  rule fails the frame with that kind. Rules out: resetting on alternation. (Claude.)
- **Child-skill failures count toward the parent rule's failure streak, and skills explore with a
  frontier `explore` primitive.** Live runs from a free spawn livelocked: a child's `failed(kind)` only
  made the calling rule ineligible for one tick, so the parent re-called the same child forever.
  Now a child's `failed(kind)` counts as one failure of the calling rule; three in a row fail the
  parent with that kind (same rule as primitives). Exploration: a new discrete-goal primitive
  `explore(target)` (block or `#tag`) walks toward the nearest frontier of the perception known map
  (known walkable cell next to unknown cells), preferring frontiers away from where it already
  searched, until perception reports the target or its tick budget runs out (`failed(not_found)`).
  Gathering skills get a rule `not near(T, R) -> explore(T)` below the mining rule. Reason: the
  architecture's "exploration follows from perception" needs a mover that makes unknown cells known;
  `observe` only looks. Rules out: a larger blind search radius in `mine`, and world-seed knowledge. (Claude.)
- **Demo-0 data path: the headless body connects through the forwarder, and the world model is fed
  only by the forwarder's tee.** The headless client writes packets (plus the minimal protocol replies
  it must send itself); it never feeds perception. Reason: decoder, world model and perception stay
  unchanged when Demo-1 swaps in the real client, and the forwarder is exercised under real load from
  Demo-0 on. Rejected: a direct connection that decodes its own packets. (User.)
- **Rule-list syntax: Praxion's own small line-based DSL** (`guard -> action`, as in session 5's
  example), parsed by a hand-written Rust parser, with three-valued guards written explicitly.
  Rejected: TOML or RON data (no parser to write, but hard to read at 20 to 30 rules). (User.)
- **Demo-0 first-night task:** wood, crafting table, wooden pickaxe, stone pickaxe and sword, food if
  animals are near, a shelter before dusk (dig into a slope or enclose itself), then wait for morning
  with reflexes on. Gate: at least 18 of 20 fresh seeds reach morning without death (session 7). (User.)
- **Demo-0 build order:** (1) version manifest, body workspace, harness skeleton; (2) harness and
  referee on a real server; (3) forwarder, decoder and world model from the spikes; (4) perception API;
  (5) headless body; (6) receipts and event log; (7) fake world and interpreter; (8) primitives and
  planner; (9) reflexes and arbiter; (10) first night on 20 fresh seeds. Steps 2 and 3 run in
  parallel; every step is tested on its own, by a model other than its writer, and live where it plays. (User.)
- **Headless body: our own thin client on `azalea-protocol`, with physics lifted from `azalea-physics`'s
  pure helpers** (`move_colliding`, collisions, `jump_from_ground`); `azalea-client` is only a reference
  for packet order. Fallback if lifting stalls: run `azalea-physics` systems in a minimal bevy `World`.
  Rejected: `azalea-client` or the `azalea` bevy app as the body (a second world model next to the
  decoder). Evidence: `docs/research/demo0-azalea-headless.md`. (User.)
- **Harness: Python, RCON plus server-log tail, a fresh world per seed under `runs/<id>/`, every fixture
  command in a JSON-lines log.** Player-dependent referee tests wait for the headless bot instead of
  opening the real client on the user's screen. (User.)
- **Third-party inputs leave `spikes/`:** the Azalea clone goes to `third_party/azalea`, the server jar to
  `third_party/minecraft/`, the toolchain shim to `third_party/toolchain` (all ignored); patches to
  `versions/patches/`; the setup script to the root `setup.py`, which also writes the machine-local
  `local/paths.toml`. Reason: product code must not depend on the throwaway spikes folder. (Claude; the
  user asked Claude to decide such things without asking.)
- **Tee stream format `PRXCAP2`** (`body/crates/tee`): the spike's capture format plus a `gap` record
  carrying the bytes the tee dropped for a connection; the reader also reads `PRXCAP1` spike captures. A
  tee subscriber that joins while a game connection is open first gets a gap for it, since it cannot
  decode a stream it joined mid-way. One subscriber at a time; a full tee queue drops tee bytes, never
  game bytes. (Claude.)
- **See-through blocks (closes the transparent-materials [O] of architecture §5.1):** a sight ray is
  tested against the block's collision boxes and passes if it misses them all. Overrides: full-collision
  blocks that are see-through (glass, stained glass and panes, tinted glass, ice, leaves, slime, honey,
  barrier, spawner, beacon) and no-collision blocks that block sight (lava, powder snow). A block is
  visible if a ray reaches its center or the center of a face bordering a see-through cell (at most 7
  rays); an entity if a ray reaches its eye, center or feet. Reason: exact for slabs, stairs and plants
  without a hand-made shape table. (Claude.)
- **Rule-list DSL details:** `skill NAME(params)`, then ordered rules `GUARD -> ACTION [until GUARD]
  [max_ticks N]`; guards use `and`/`or`/`not`, comparisons and typed fact calls evaluated in Kleene
  logic (unknown propagates); actions are a primitive call, `do skill(...)`, `done` or `fail(kind)`.
  The loader rejects unknown names, type errors, more than 30 rules, cycles, composition depth over 2
  and unreachable rules. Reference: `docs/rule-lists.md`. (Claude.)
- **Reflexes and arbiter for Demo-0:** a reflex is a pure, priority-ordered function over perception; a
  `human_floor` reflex acts only after its trigger has been visible for 3 ticks (150 ms), a `lethal_fast` one
  (creeper within 3, arrows, lava, drowning) on the first tick. Ten typed settings live in
  `body/config/reflexes.toml`. The arbiter bumps the generation and preempts the interpreter on a reflex,
  eats at a safe point on the soft hunger threshold and preempts on the hard one, and switches within a tier
  only when gain > k x loss (k = 2) with no `until` active. An Enderman look guard applies to every look.
  (Claude.)
- **Perception keeps a known map; the planner plans on it without rays.** Each tick perception spends its
  leftover ray budget (never the reflex reserve) sweeping nearby unknown or stale cells and records what it
  sees, with the tick, in a bounded per-section map; a hidden cell is never updated. The planner reads that
  map (`Known(state, age)` or `Unknown`), so a plan costs no rays and unknown cells form a real frontier.
  Reason: planning through per-node ray queries spent the whole budget and ended 79 of 100 test plans early.
  Plans also track placeable blocks left, and water is swimmable (lava stays impassable). (Claude.)
- **Item tags in skills, all recipes from data.** Facts and primitive arguments accept item and block tags
  (`count(#logs) >= 1`, `mine(#logs)`), resolved from the 26.3 data pack's tag files; `praxion-gamedata`
  generates every crafting recipe (shaped and shapeless, with tags) from the server jar instead of a
  hand-copied oak-only table. Reason: a fresh seed may have only birch, spruce or other woods.
  `ControlFrame` gains `select_slot: Option<u8>` (the hotbar number keys). (Claude.)
- **Interpreter readings where §6.1 is silent:** a failed primitive blocks its rule for the tick and is
  retried, but three consecutive failures of the same rule return `failed(kind)` from the skill; a child's
  `done` lets the parent re-select on the next tick; `until` on a skill call is bounded by 1200 ticks;
  `observe` is a discrete primitive with evidence `FactKnown` and a timeout. Craft effects come from the
  26.3 recipe data and mine/attack drops from the loot tables, never by hand. (Claude, after review t7.)
- **The headless actuator reads the raw store for physics, read-only.** It plays the game client, which
  also simulates movement with full world knowledge; decision-making layers (reflexes, interpreter,
  planner) still see the world only through perception. (Claude.)
- **`until` reads as English and composition depth depends on the author.** `ACTION until G` keeps the
  rule committed until G becomes true (or its own guard falls, or `max_ticks`). Hand-written core skills
  may compose to depth 4 so each stays at 2 to 6 rules; LLM-written skills keep the depth-2 cap at
  admission. Amends the depth-2 session pick (architecture §6), which made core skills inline each
  other's steps (11 repetitive rules for `get_stone_pickaxe`). (Claude.)
- **The level-1 fake world is a Rust crate** (`body/crates/fakeworld`), not Python under `harness/` as
  architecture §10 sketched: the interpreter is Rust and level-1 tests must run in seconds through the
  same `Facts`/`Actuator` traits the real body implements. (Claude.)
- **Secrets live in an ignored `.env`** (`TYPESAFE_API_KEY`, `OPENAI_API_KEY`); `.env.example` documents
  the names. The user fills in the keys. (User.)

## 2026-11-01

All four on Claude's recommendation, after the 2.7 Windows runs (`docs/sessions/02-body-connection.md`).

- **The product-code gate is split.** Demo-0 code may start now: every experiment it depends on has
  passed (E1 forwarding latency, E2 decoding with the local Azalea patches, E3 decode and world update,
  E4 measured, E13 loop timing with the process settings below). Demo-1 code (SendInput actuator,
  take-over, indicator) waits until E8, E10, E11, E12, E15 and E16 have passed. Supersedes "first product
  code is gated on the passed 2.7 experiments" for Demo-0 only. Rejected: finishing every hands
  experiment first (Demo-0 has no SendInput, so they cannot change it). (User.)
- **Decoder: Azalea at the pinned `26.3` commit with local patches** (`spikes/patches/`), applied by
  `spikes/setup.py`. The patches carry 26.3 layouts read from the server's own classes
  (`level_particles`, `update_advancements`). Fallback stays MCProtocolLib. (User; E14's written
  comparison is reduced to this entry.)
- **The body's loop process applies `nothrottle`, `high` and `mmcss` to itself** (opt out of power
  throttling including timer-resolution throttling, `HIGH_PRIORITY_CLASS` with a time-critical loop
  thread, MMCSS task "Games"). Process-local only; system settings are never changed. Reason: E13 p99
  < 0.9 ms with them, default settings missed the 2 ms threshold in 2 of 3 runs. (User.)
- **Key holds are counted in client ticks, not milliseconds** (Demo-1). The actuator releases a key
  after the proxy has seen the wanted number of `client_tick_end` packets with the key down. Reason: E7
  fixed 50 ms holds gave 97%, with misses and doubles depending on tick phase. (User.)
- **After every mouse grab the actuator sends a 1-count primer, and every turn is verified from the
  proxy stream** (Demo-1): the first relative event after a grab is dropped (seen 4 times); a turn that
  falls short is completed from the observed rotation. (User.)

## 2026-10-31

- **Interface technology: a local web interface.** The Python brain serves a small web page on
  loopback; the command box, reports, live view and memory viewer are a browser tab. The
  "Praxion is active" indicator stays a separate small window fed by the body (2.4). Rejected: a
  desktop app (Tauri), which adds a second stack and packaging work. (User, on Claude's recommendation.)
- **Lesson trigger: a failure-repair pair.** The LLM proposes a lesson when the same failure kind
  has occurred in two different contexts and a later repair or revision succeeded. Code still
  accepts it only after 3 proxy-verified successful traces. Rejected: a proposal after every skill
  admission (noise, cost). (User, on Claude's recommendation.)
- **No commercial parts: everything is Apache-2.0.** The user has no commercial purpose, so the
  open-core split is dropped and no part stays closed. Supersedes "commercial parts closed" of
  2026-10-29 and closes the "which parts are commercial" open point. Dependencies must still carry
  licences compatible with Apache-2.0. (User.)
- **`spikes/` source is committed; downloaded binaries are not.** Spike code and reports go into
  the repository now that the licence is chosen; Mojang jars, build output and captures stay in
  `.gitignore`. (User, on Claude's recommendation.)
- **Wiki RAG is built.** The user states there is no commercial purpose, so the non-commercial
  clause of CC BY-NC-SA does not block use. The rule stays that wiki text is never distributed
  (the index is local; the licence field and the build check remain as a cheap guard while the
  repository is public). Closes the wiki RAG [O] of session 12. (User.)

## 2026-10-30

- **Rust toolchain for the Azalea `26.3` branch: `nightly-2026-08-20`, no extra flags.** The branch
  (commit `8041a14`) does not build on nightlies near its commit date (unstable `generic_const_exprs`
  breakage that Azalea `main` fixed later); this nightly builds the decoder crates unchanged.
  Rejected: `nightly-2026-09-25` plus `-Znext-solver=coherence` (Azalea `main`'s own fix), which adds a
  compiler flag the build depends on. Revisit when Praxion follows Azalea `main` to 26.3. Evidence:
  `docs/sessions/02-body-connection.md` "Azalea build". (User, on Claude's recommendation.)

## 2026-10-29

- **Architecture draft 2 accepted.** The user accepted every item that draft 2 tagged [P], in four
  groups: perception (query result type, cache invalidation, ray budget with a reflex reserve,
  entity cap) and actuator (frame fields, stale-frame drop, `HoldGuard`, movement style as the only
  look producer); port (liveness ping, command expiry, resync overflow, generation details), the
  stop-cause table and the planner (goal types, frontier, extra moves, primitive records as the
  single source); interpreter (`until` bounded by its own guard and `max_ticks`, call-stack
  composition) and skills (size limits, obtain-X solver, storage outside the repository,
  low-privilege writer, hidden fixtures, suspected-primitive reports to the user); safety and
  structure (nonce, hook probe, watchdog, release order, stuck-key escape, gesture details,
  "needs you" cue, ruleset hash, UUID filter, remote pre-shared key, blocked-input stop,
  environment checks that refuse; first slices Demo-0/1/2 and the trimmed layout, latency classes,
  call classes, ticks as the arbiter unit, code-level heartbeat diff, prompt caching, state hash,
  receipt settlement, the session minor picks, wiki RAG flag and NC build check, test-level
  matrix, manifest extras, staged chunk cache, corpus not committed if the terms forbid it). Details
  in [architecture.md](architecture.md); the remaining [O] items stay open.
- **"Never inject into a menu" means the pause, options, social and death screens only.**
  Inventory, chest, crafting-table and other container screens are normal play and use the
  absolute menu mode. Clarifies the dedicated-server pause heuristic. Draft-2 review (DeepSeek, finding 2).
- **Receipt attribution: tick `>=` plus a causal match.** A receipt counts only if its evidence
  tick is at or after the intent's start tick on the body's single tick source, and the action's
  sequence number (or, for non-block actions, the injected-event nonce) matches the intent;
  otherwise it stays `unconfirmed`. Amends the epoch rule ("after the start tick") of the
  per-action receipts decision. Draft-2 review (DeepSeek, finding 4).
- **An unanswered high-risk question times out after 60 s:** Praxion cancels that command, says so
  in the interface and in chat, and resumes the previous task. Adds a bound to session 9's
  "high risk, ask first". Draft-2 review (DeepSeek, finding 22).
- **Body port v0 rules.** `hello{port_version, caps}` handshake, refused on mismatch; a `seq` on
  every body message; snapshot-plus-delta resync after a brain restart; bounded queues that
  coalesce per key and send `dropped(n)` when they drop; the body alone increments `generation`
  (owner change, take-over, new user command, reflex preemption) and rejects directives of an old
  generation. Transport: length-prefixed JSON over loopback TCP (fits the remote-brain decision,
  logs stay readable); MessagePack only if measurement demands it. Settles the 2.5 minor pick
  "format chosen at first code". Review round 1, R1-B7.
- **The body's append-only event log is the single source of truth.** Memory, ledger progress,
  skill statistics and reports are projections rebuilt from it; the brain writes proposals only
  (candidates, lesson text, ledger intents). Answers "who owns the memory file" (architecture
  risk 10). Review round 1, R1-M4.
- **One intent stack, owned by the body** and persisted in the event log: entries
  `{intent_id, tier, skill_ref, args, state: running | suspended | frozen_for_carry_on | done}`.
  The brain pushes and pops through the port; the project ledger keeps only long-horizon goals
  that point to intent ids. Review round 1, HOL-8.
- **Rule-list guards are three-valued (Kleene AND/OR).** A rule whose guard is `unknown` is not
  eligible and offers an `observe(x)` subgoal; no eligible rule gives `failed(no_rule)`, which wakes
  the brain; an in-flight discrete action blocks until its receipt or a timeout; `until` is a
  commitment guard. Specified as a truth table and a state diagram in the architecture. Rules out
  `unknown = false`. Review round 1, R1-M2.
- **One versioned contract record per primitive:** effects generated from game data (produces,
  consumes, changes, reveals, duration, tool tier, probability) plus a hand-written precondition
  block (reach, support, free space, light, line of sight), cancellation class, required evidence
  and cost. A fake-world conformance test compares declared with measured effects. The LLM writes
  a step program that is compiled to a rule list. Review round 1, R1-M7.
- **Receipts use per-action evidence contracts**, stored as data in the primitive record: dig →
  block is air; place → block state; pickup → inventory delta; move and look → `unrefuted`; slot
  click → client packet plus no correction. Epoch rule: a receipt counts only if its tick is after
  the intent's start tick. Input-to-packet attribution is measured in E9. Review round 1, R1-M1.
- **Planner contract:** a time budget per segment; statuses `success | partial | timeout | noPath`;
  planning state includes hotbar, best tool and protected blocks; turning the camera then moving
  is a two-phase primitive. Only Azalea's A* core and cost constants are reused; place, bridge and
  pillar moves are written by us (to be confirmed in the session 6 spike). Review round 1, R1-M10.
- **Version pinned: Minecraft Java 26.3 (protocol 777).** Client, server jar, Azalea commit and
  Rust toolchain are pinned together in one manifest until Demo-1 passes; every upgrade needs the
  user's approval. Reason: Mojang ships about quarterly and Azalea lags (its README names 26.2).
  Replaces the rolling "newest stable at coding start" rule of 1.1 for now. Review round 1, Q4,
  [reviews/2026-10-29-arch-r1/synthesis.md](reviews/2026-10-29-arch-r1/synthesis.md) R1-B2.
- **Hard requirement: no body or decoder crash may drop the user's game connection.** The
  forwarder is its own minimal process (forwards bytes, no decoder, no game logic) and tees a copy
  to the body; decoder and world model live in the body. The "isolated thread inside the body"
  fallback is ruled out; on a latency miss the decoder may move into the forwarder, never the
  forwarder into the body. Restores D1. Review round 1, Q3, R1-B3.
- **The 2.7 experiment plan is amended** as listed in synthesis.md "Changes to the 2.7 experiment
  plan": a desk round 0 (version manifest, read the 26.3 SDL input path, stock-client capture
  corpus); E1/E3 latency judged on Windows under co-load with p99 and p99.9 (macOS judges
  correctness only); E5 drops the removed raw-input axis and checks injected relative moves under
  SDL3; new E15 (integrated tick) and E16 (protocol-body fidelity); a missed threshold still stops
  the work, and the report carries a fallback shortlist for the user. Reason: 26.3 replaced GLFW
  with SDL, and nothing measured the whole loop. Review round 1, Q11.
- **During play the PC is operated only physically.** Remote-control tools (Parsec, AnyDesk,
  Remote Desktop) are unsupported while Praxion plays. Reason: take-over detection stays simple,
  and Remote Desktop already blocks raw relative input (session 2). Review round 1, Q8.
- **Actuator seam: a per-tick `ControlFrame`** (held controls, look delta or target, clicks and
  slot actions). Reflexes, planner, primitives and movement style produce only frames; SendInput,
  the headless packet client and a possible future client mod are interchangeable implementations
  below it. Cancellation classes stay on actions (session 4); the frame carries only held state.
  Rules out primitive-level abstraction (every primitive per body). Review round 1, R1-B7.
- **Command gesture.** While Praxion plays, a physical press of the bound chat or command key
  releases all keys except danger-held ones (session 4) and waits while the chat screen is open;
  if the next chat the client sends is a prefixed command, Praxion takes it and keeps control;
  anything else is a normal take-over. Refines the 2.4 rule "any key takes control back".
  Tested in E12. Review round 1, Q1, R1-B5.
- **Clicking into the interface's command box counts as a take-over.** The indicator never takes
  focus; the command box is used while Praxion is not playing. Review round 1, Q2.
- **Safety does not depend on the brain or the network.** (a) The body parses a fixed list of
  control verbs (stop, cancel, status, carry on, hand-over) with a local grammar before any brain
  call; Jev maps only other commands. (b) The indicator is fed by the body. (c) On brain silence
  the body finishes the current primitive, runs a hand-written shelter or retreat skill, then
  holds with reflexes on. (c) amends 2.5 ("stand still with basic survival reactions").
  Review round 1, Q5, R1-B6.
- **Dedicated-server trade-offs accepted.** No Esc pause; while Praxion is stopped, waiting for
  "carry on" or unfocused, the world keeps ticking and the character may be defenceless (covered
  by "death is acceptable"). Added: a pause heuristic (no client movement packets for N ms while
  active means a safe stop; never inject into a menu) and a server setup checklist. Review round 1, Q6, R1-M6.
- **Movement and looking are verified as `unrefuted`:** no server correction within N ticks.
  World changes still need the server's word (level 2) and expected effects level 3. Refines
  "an action succeeds only with the server's word" for client-authoritative movement. Review round 1, Q7, R1-M1.
- **Cost brake: warn, cap and one hard stop.** Warn above the user's threshold as before; add at
  most 3 repair attempts per goal, a global LLM call-rate breaker that degrades to code-only after
  repeated failures, and one high hard stop set by the user that pauses autonomous LLM work while
  reflexes and the core keep running. Amends "never cut automatically" (session 8). Review round 1, Q9, R1-M9.
- **Skill admission evidence comes from the headless protocol body**, after it passes the E16
  fidelity check. The k=3 practice run is a smoke test: 3/3 on the skill's own fixture, no
  regression on pooled fixtures, at least one success on a held-out fixture from a different seed
  or biome, no world-specific literals. Statistical confidence comes from probation on the test
  server (Wilson bound, at least 8 uses). Reason: SendInput needs the foreground, and 3/3 has a 95%
  Wilson lower bound of only 0.44. Review round 1, Q10, R1-M8.
- **World backup before every autonomous session.** Zip the world folder before each session,
  always shut the server down cleanly (save flush), keep the last N backups. Review round 1, critic 5.
- **Raw world data is readable only by the perception module, enforced by tooling.** The body
  stores everything the decoder receives (a chunk arrives once, so filtering at ingest would lose
  blocks exposed later); visibility is enforced at a deny-by-default query API. A module boundary
  and a lint rule stop any other module from reading the raw store. Remembered facts live in a
  separate memory with age and source. Rules out filtering at ingest. Review round 1, R1-M3, critic 4.
- **Languages: Python plus a Rust core.** Python for the brain (LLM and Jev calls, skills); Rust
  for the wire and input path. Rust chosen over C++ (Azalea decoder; user confirmed). Decoder
  library Azalea, to be verified in experiment 2.7. Exact boundary decided in 2.5.
  Session 2.6, [sessions/02-body-connection.md](sessions/02-body-connection.md).
- **Hands: Windows `SendInput`, scan codes, key bindings read from `options.txt`.** No driver-level
  injection, so Praxion can tell its own input from the user's. Session 2.3.
- **Mouse has two modes.** In the world: relative moves, calibrated in a closed loop against the
  rotation the proxy sees. In menus: absolute cursor positioning, with wrong clicks caught by the
  slot number in the container-click packet. Session 2.3.
- **Focus loss: stop and tell the user.** Release all keys, stop safely, never take focus back by
  force. Praxion's own windows never take focus. Session 2.3.
- **Take-over ignores tiny mouse jitter.** Any key or mouse-button press, wheel turn or clear
  mouse movement takes control back; very small drifts are ignored (threshold tuned in
  experiment 2.7), so the user may rest a hand on the mouse. Session 2.4. *Refined 2026-10-29 by the command gesture (Q1).*
- **After a take-over, the interrupted task waits.** Praxion remembers it, does not play until the
  user says "carry on", then re-reads the world and re-plans if it changed. It never resumes from
  its own memory of the state. Session 2.4.
- **"Praxion is active" indicator: a small separate window outside the game area**, never
  stealing focus, nothing drawn over the game. Answers the open point from session 1. Session 2.4.
- **Brain and body are separate programs** (Python brain, Rust body) joined by a local connection:
  the body survives a brain crash, the brain can restart without dropping the game connection,
  and tests and replays use the same interface with a fake world. Session 2.5.
- **The brain may run on another computer.** Default is this PC only (loopback); network access
  needs an explicit key. Session 2.5.
- **If the brain goes silent, the body stops safely and tells the user:** finishes the current
  step to a safe point, stands still with basic survival reactions on. Which reactions: session 7.
  Session 2.5. *Amended 2026-10-29 by "Safety does not depend on the brain" (Q5).*
- **Seeing = line of sight in every direction within a distance limit** (about 64 blocks,
  tunable); a wall hides what is behind it. Camera direction is ignored. Session 3.
- **Hidden data: what the F3 screen shows is allowed, everything else is denied** (a mob's exact
  health, the dragon phase counter). Every privileged read goes to a ledger. Session 3.
- **The live world state lives in the Rust body**; the brain asks short queries and receives
  events, never raw chunks. Reason: the body must survive a silent brain and reflexes need the
  state within 5 ms. Session 3.
- **Remembered facts carry age and source, are re-checked when in line of sight, and Praxion tells
  the user when it is unsure.** Session 3, [sessions/03-perception-world-model.md](sessions/03-perception-world-model.md).
- **An action succeeds only with the server's word** (level 2 for world changes, level 3 when an
  effect is expected). No confirmation gives `unconfirmed`, then re-read the truth, then retry.
  Session 4. *Refined 2026-10-29 for movement: `unrefuted` (Q7).*
- **Unverifiable tasks (not computable within the perception policy) are marked `unverifiable`,
  never counted as successes.** Session 4.
- **Safe take-over exception:** each action declares a cancellation class; in a dangerous moment
  (bridge-edge sneak, mid-air jump) its needed keys stay held until the user touches them or the
  moment passes. Refines the 2.4 rule "release all keys". Session 4,
  [sessions/04-action-verification.md](sessions/04-action-verification.md).
- **Skills are teleo-reactive rule lists (`condition -> action`) that the Rust body interprets**, so
  the body keeps playing while the brain thinks. Delicate micro behaviour stays in hand-written
  Rust primitives. Session 5.
- **LLM-written skills only combine existing primitives and skills.** Narrows the 2026-10-21
  decision "LLMs may write code, for skills": they write rule lists, not primitives. Session 5.
- **The compiler derives the guards** from primitive effects (generated from game data); the LLM
  writes steps and a goal. Session 5, [sessions/05-skills.md](sessions/05-skills.md).
- **Jev is the middle tier and is wired in by measurement:** only on options the code lists,
  deadline above about 1 s, compared with rules and the LLM at each decision point. Reflexes and
  anything under 1 s never depend on Jev or any hosted call. Session 8.
- **The LLM is called by events (closed list of causes plus a ~60 s heartbeat), not by a clock.**
  Session 8.
- **Cost brake: track and warn above a user-set threshold; never cut automatically.** Every call
  class has a deadline and a fallback (stop safely). Session 8. *Amended 2026-10-29 by "Cost brake: warn, cap and one hard stop" (Q9).*
- **Smoothness is measured as a starvation gap** (goal active, no declared wait, no packets), not
  as the longest gap without input. Amends measure 3 of the 2026-10-23 success measures.
  Session 8, [sessions/08-jev-llm-brain.md](sessions/08-jev-llm-brain.md).
- **Reinforcement learning is a noted idea (see `notes.md`), not part of the first demo.**
  Primitives stay swappable. Session 8.
- **Arbiter tiers: survival reflex, then the user's command, then maintenance, then own
  projects, with a soft and a hard threshold per need.** A need waits for the command's next safe
  point until its hard threshold promotes it. Same-tier switches need a clear gain. Session 7.
- **Reflex settings are a fixed default (about 10 typed fields) the user can edit; the LLM does
  not tune them in the first version.** Session 7.
- **First reflex set: the first night, extended at every ladder rung**, tested with a coverage
  matrix judged by the test harness, not by Praxion. Session 7,
  [sessions/07-reflexes-arbiter.md](sessions/07-reflexes-arbiter.md).
- **Commands come from in-game chat with a prefix and from the separate interface.** Voice is
  left for later. Session 9.
- **Ambiguous commands: low risk, guess, start and ask without blocking; high risk, ask first.**
  Session 9.
- **Reports go to the separate interface first; in-game chat only short acknowledgements at safe
  moments; every claim cites a receipt.** Session 9.
- **A new command suspends the old one and the old one resumes afterwards; stop and cancel always
  work.** Session 9, [sessions/09-commands.md](sessions/09-commands.md).
- **A path may change the world anywhere (dig, bridge, tower) but must choose a sensible route**
  (world-changing moves cost more). Hard limits stay: ruleset-protected blocks, no digging straight
  down or into unknown blocks. Session 6.
- **Stuck handling is bounded:** retry counters, loop detection, blacklist with reset conditions,
  typed `unreachable`, then an event for the LLM. Never `/tp` or any operator command. Session 6.
- **Natural movement is a separate layer above the planner** (tuned in session 13). Session 6,
  [sessions/06-movement.md](sessions/06-movement.md).
- **A new skill goes through staged admission: static checks, then a practice room (a world copy
  on a test server, 3 runs against a no-skill baseline), then probation, then active.** Never first
  tried in the user's world. Session 11.
- **A skill learned in Praxion's own play and passing the gate becomes active without asking; a
  skill taught through chat needs the user's approval.** Session 11.
- **The user's own play is recorded locally now; learning from it comes later.** Session 11,
  [sessions/11-skill-learning.md](sessions/11-skill-learning.md).
- **Praxion chooses its own goals with a free, Voyager-style curriculum;** every goal carries a
  completion condition, and the ladder stays the yardstick. The user chose this over "the ladder
  as the spine". Session 10.
- **After a hand-over with no command: resume the suspended task, else choose a goal and say so.**
  Session 10.
- **Big risky steps (Nether, End, dragon) are announced with a short window (about 30 s) to say
  stop.** Session 10.
- **Long projects live in a project ledger on disk; progress is recomputed from the world after a
  restart.** Session 10, [sessions/10-autonomy.md](sessions/10-autonomy.md).
- **The first memory is the smallest one, built in stages:** a JSON Lines file per world plus an
  in-RAM index, places and containers, an event log with a read-only viewer, a briefing with a
  token cap. SQLite, vectors, lessons and the chunk cache wait for a measured trigger.
  Session 12.
- **Lessons come when a trigger fires, are general across worlds, and are proposed by the LLM and
  accepted by code after 3 proxy-verified successful traces.** Session 12.
- **Game knowledge: the Minecraft Wiki text is indexed and a RAG is built** (the user's choice
  over "exact data and the LLM's knowledge"). The text is CC BY-NC-SA: the index stays local and
  undistributed, and the licence conflict with an open-core model stays open. Session 12.
- **The game memory is separate from Mnema and the hub.** Session 12,
  [sessions/12-memory-rag.md](sessions/12-memory-rag.md).
- **Movement style is a general human style, not fitted to the user.** The recording of the
  user's play serves as the evaluation reference; personalising stays a later option. Session 13.
- **Human limits without fatigue:** reaction floor about 150 ms, human aim statistics, no faked
  breaks; lethal-danger reflexes may be faster. Session 13.
- **Praxion answers in the language the user writes in; short, calm, informative tone; name and tag
  `Praxion`, configurable.** Session 13,
  [sessions/13-natural-feel.md](sessions/13-natural-feel.md).
- **Tests run at three levels (fake world, protocol test body, real body), most at the bottom.**
  Session 14.
- **Evidence standard: many fresh seeds, confidence intervals, analysis written before the
  runs; a hand-picked best run is labelled as such.** Session 14.
- **Observability: a live window in the separate interface plus a replayable event log; every claim
  cites a receipt id.** Session 14.
- **Fast tests on every change, level 2 and 3 tests nightly or on request.** Session 14,
  [sessions/14-testing.md](sessions/14-testing.md).
- **The brain runs on the same Windows PC by default.** Session 15.
- **One repository; the layers are enforced by tooling.** Session 15.
- **API keys live in the operating system credential store; only the brain holds keys.**
  Session 15.
- **Licence: Apache-2.0 for the open core, commercial parts closed.** `LICENSE` added; which parts
  are commercial is still open. Session 15,
  [sessions/15-infrastructure.md](sessions/15-infrastructure.md).
- **Experiments (2.7): thresholds accepted; Windows tests at the physical PC; spike code in
  untracked `spikes/` until the license is chosen.** A missed threshold stops the work and goes to
  the user; no automatic route change. Session 2.7.
- **Mineflayer is the reference model for the body.** Not a dependency (see 2026-10-21). We take
  its ideas: the `controlState` held-keys table, rate-limited and quantized look, `human.js`
  calibration (for session 13).
- **Proxy: a separate process that forwards bytes untouched and decodes a copy.** The forwarding
  path never encodes packets, so read-only holds structurally. A decoder that loses sync marks
  perception invalid and Praxion stops safely; the game is unaffected. Rejected: terminate and
  re-encode both connections (a codec bug could corrupt the game). Session 2.2. *Clarified 2026-10-29 (Q3): the forwarder process has no decoder; it tees the copy to the body, which decodes.*
- **Game data comes from the Mojang data generator reports** (blocks, packets, registries).
  `minecraft-data` is the backup only. Session 2.2.

## 2026-10-23

- **Success measures:** milestone ladder to the Ender Dragon over many fresh seeds; a
  proxy-verified command suite; smoothness (never freeze while an LLM thinks, instant take-over);
  naturalness (human movement statistics and a blind test); hosted cost per hour, no cap yet.
  Targets are set after baseline measurements. Course project; schedule does not constrain
  design. Session 1.6.
- **First demo:** fresh survival world; the opening scenario is hand-over, a verified chat
  command, instant take-over, and an autonomous first night. Capability target is the whole game
  up to the Ender Dragon. Hand-over by chat command, global hotkey or a separate interface.
  Must be flawless; audience: the community and the user's professor. Session 1.5.
- **Goals when alone:** the user's projects first, then Praxion's own ideas. Goals come from
  in-game chat, a separate interface, or Praxion's own inference in autonomous mode. Death is
  acceptable. Rules (protected areas, items, limits) may be written with an LLM; proposal: code
  enforces them deterministically after the user confirms. Plays toward finishing the game; an
  LLM may decide to continue past it. Session 1.4.
- **Body direction: eyes on the network, hands on the keyboard.** A read-only local proxy
  between the user's unmodified client and a private local server gives exact perception;
  OS-level keyboard and mouse input drives the real game window. A client mod is the fallback.
  Details in session 2. Session 1.5.
- **Praxion never changes the user's game settings.** It reads them and calibrates itself.
- **The PC is dedicated to the game while Praxion plays.**

## 2026-10-22

- **First demo: Praxion plays the user's own character** (player-bound), watched on the user's
  own screen with no visible change when control passes. Touching mouse or keyboard, or closing
  Praxion, takes control back. A private local server counts as singleplayer. Implementation
  effort is not a criterion; quality is. Body route still open. Session 1.5.

## 2026-10-21

- **Survival, Normal difficulty, zero cheats.** The agent's action and perception APIs expose only
  what a player can do and see; test setup goes through a separate, logged harness. Preference:
  avoid mods; the body route is decided in session 2. Session 1.3.
- **Minecraft only; the second-game goal is dropped.** Layering stays for Minecraft's own needs
  (two bodies, version packs, offline fake world): agent layers do not import body or
  version-pack layers, enforced by tooling. Session 1.2.
- **Singleplayer only, for now.** Multiplayer servers are out of scope until decided otherwise.
  Session 1.3.
- **Java Edition, on Windows.** The demo targets the newest stable release; broad version
  support comes later. Vanilla first; block, item and recipe knowledge comes from data, not
  code. *The "newest stable" rule is replaced for now by the 26.3 pin (2026-10-29, Q4).* Version-specific code must stay in one layer so later versions are added, not patched
  in. Rejected: Bedrock (no client mods, blocks player-bound mode). Session 1.1,
  [sessions/01-product-scope.md](sessions/01-product-scope.md).
- **The decision layer uses TypeSafe Jev, with LLMs behind it.** How Jev and the LLMs split the
  work is open and is discussed in session 8 ([discussion-plan.md](discussion-plan.md)).
  The user is open to a better proposal.
- **Models are hosted, not local.** Reason: decision quality.
- **LLMs may write code**, for skills and similar.
- **Mineflayer is not a dependency.** Praxion is a separate project; ideas, and code where
  licenses allow, may be taken from existing systems
  ([research/systems-review.md](research/systems-review.md)).
- **Working method:** every topic is discussed in depth, one session per topic, with notes.
  The user must master each topic and its technical structure.
