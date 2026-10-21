# Source-level review of existing Minecraft agent systems

Research date: 2026-10-21. Method: shallow clones of each repository, read at source level.
Complements [game-agents.md](game-agents.md), which surveys papers and results; this file
records how the systems are actually built and where they break. Nothing here is a decision.

Commits read: mineflayer 4.39.0 (`7b4bb8d`, 2026-10-10), mineflayer-pathfinder, prismarine-physics,
Voyager, Odyssey, mindcraft (`f6a9556`, 2026-10-02), altoclef (`af22e3b`, 2023-12), Baritone,
Craftonomous (`7d59982`, 2026-08), minecraft-agent-swarm (`229c4ee`, 2026-09).

## 1. Summary

1. **A system can only learn the skills it can verify.** Voyager's curriculum prompt (rule 8)
   forbids placing, building, planting and trading tasks because its critic can only check the
   inventory. The definition of a skill must include how it is verified.
2. **"The action returned" is not "the world changed".** Mineflayer's `dig()` predicts the dig
   time, sets the block to air locally and resolves; it never waits for the server. Mindcraft's
   `collectBlock` counts a block as collected when the promise resolves, without checking the
   inventory. Systems that get this right: altoclef (`isFinished` reads the inventory) and
   Craftonomous (measures the inventory delta).
3. **Two execution models exist.** Script model (Voyager, mindcraft, Odyssey): an async function
   runs top to bottom. Reactive tree model (altoclef): every tick, `onTick` answers "which subtask
   should run now?" from the current world. The tree recovers by itself after interruptions; the
   script model is easier for an LLM to write.
4. **The mature fast-decision layer is altoclef plus Baritone.** Chains compute a numeric
   priority each tick from the situation (a creeper's priority rises with its fuse), the executor
   reports whether stopping is safe right now, and temporary processes (pause to eat) do not take
   control from permanent ones. Mindcraft's fixed-order mode list is the simple version.
5. **Stuck detection is everyone's weak spot; expectation-based detection works best.** Baritone
   cancels a movement that exceeds its own cost estimate by 100 ticks. Altoclef uses generic
   progress checkers and blacklists unreachable targets, resetting the counter when the bot gets
   closer or gets a better tool. Voyager teleports with `/tp`.
6. **Voyager's results rely on operator commands.** `/tp` when stuck; after every step the
   crafting table and furnace are removed with `/setblock` and returned with `/give`;
   `keepInventory true`; `doDaylightCycle false`. Its numbers are not a survival baseline.
7. **Reliability statistics mislead unless failure causes are separated.** minecraft-agent-swarm
   retired `build_nether_portal` at 1/11 and blocked its whole Nether strategy; it retired
   `wood_run` at 0/9 when the cause was "no tree in sight", a fact about the world, not the skill.
   Craftonomous's typed failure kinds and Wilson lower bound address this.
8. **Omniscient perception is the silent default.** Mineflayer can read every loaded block,
   including those behind stone. Craftonomous's survey of 52 projects found only two
   (hermescraft, MineLand) that restrict senses on purpose.

## 2. Licenses

| System | Language | License | Read for |
|---|---|---|---|
| mineflayer | JS | MIT | Protocol-level action layer, verification, physics loop |
| prismarine-physics | JS | MIT | Vanilla player physics |
| mineflayer-pathfinder | JS | MIT | Path planning and execution |
| node-minecraft-protocol | JS | BSD-3 | Dependency only |
| Voyager | Python + JS | MIT | Learned skill library |
| Odyssey | Python + JS | MIT | Hand-written skill library, tech-tree solver |
| mindcraft | JS | MIT | Reflex modes, action manager, LLM code loop |
| altoclef | Java | MIT | Reactive task tree, priority chains, world memory |
| Baritone | Java | **LGPL-3.0** | Pathfinding, process arbitration, cached world |
| Craftonomous | TS | MIT | Skill contract, perception provenance, reliability |
| minecraft-agent-swarm | TS | MIT | Generated-skill pipeline, retirement lessons |

Code from MIT/BSD sources may be reused with the license text and copyright line kept.
Baritone is LGPL-3.0: ideas only; copying code would bring derivative-work obligations.

## 3. Per-system findings

### 3.1 Mineflayer core (4.39.0)

**Plugin model.** `createBot` returns one `EventEmitter`; every plugin is `function(bot, options)`
that mutates it (`bot.dig = dig`). No declared dependencies between plugins; order is the object
order in `lib/loader.js`. Plugins read and write each other's fields (`bot.lastDigTime`,
`bot.heldItem`). Version differences are handled by feature flags from minecraft-data
(`bot.supportFeature('blockPlaceHasHeldItem')`) and an if/else chain per packet shape.

**Task primitive.** `createTask()` returns `{promise, finish, cancel, done}`.
`onceWithCleanup(emitter, event, {timeout, checkCondition, signal})` waits for an event with a
timeout and cleanup. `AbortSignal` support is recent; most actions take no cancel signal.

**What "done" means: three different things, not distinguished by the API.**
- `dig`: the client predicts the dig time (`block.digTime(...)`), sends "finish" when it elapses,
  then sets the block to air locally (`_updateBlockState(pos, 0)`). The promise resolves on that
  local update. If the server refuses (protected region, anti-cheat), the block comes back later,
  but the caller already saw success.
- `placeBlock`: waits for the server's block update (5 s timeout) and throws "Server refused"
  if the block type did not change. Real verification. Source comment: the server's reply
  carries no request id, so concurrent placements at one position cannot be told apart.
- Inventory, ≤1.16.5: explicit `transaction` accept/reject packets, with a queue that tolerates
  out-of-order replies.

**Sequence numbers (1.19+).** Mineflayer sends a `sequence` with dig/place/use packets but never
handles the server's `block_changed_ack` (no listener in `lib/`). A client that does can attribute
each block action's result exactly.

**Inventory, 1.17.1+.** The client predicts a click's result and sends `changedSlots`; the server
only answers when the client is stale, so an ignored click is never reported. Mineflayer's
`syncWindow` sends a no-op click with an impossible `stateId: -1` to force the full window state
back. A general "re-read the truth" primitive.

**Empirical server knowledge** worth keeping: wait after digging before clicking the hotbar
(`DIG_CLICK_TIMEOUT`); special waits for the crafting result slot; delay the teleport confirm by
1,500 ms after respawn to avoid "Invalid move player packet" kicks.

**Single global slots.** `bot.stopDigging` is reassigned per call; a second `dig` cancels the first.

### 3.2 Physics and movement (mineflayer `physics.js`, prismarine-physics)

- Movement is client-authoritative: the client simulates vanilla physics and sends positions;
  the server corrects with a `position` (teleport) packet that the client must confirm.
- The lowest movement primitive is keyboard-level:
  `controlState = {forward, back, left, right, jump, sprint, sneak}`, set every tick.
- Fixed 50 ms tick with an accumulator ("fix your timestep"), at most 4 catch-up ticks; extra
  ticks are dropped. Source warning: physics is inaccurate on Windows because of the 15.6 ms
  timer resolution.
- Look rotation is rate-limited per tick (vanilla yaw speed) and quantized to mouse sensitivity
  steps ("to bypass anti-cheat GCD checks").
- prismarine-physics (~1,000 lines, MIT) is a near-pure `simulatePlayer(state, world)`, used both
  for execution and for forward simulation by the pathfinder. A porting candidate.

### 3.3 mineflayer-pathfinder (~2,900 lines)

- Two levels: A* over a block graph, then per-tick control choice by forward physics simulation
  (`canStraightLine`, `canSprintJump`, `canWalkJump` simulate up to 200 ticks).
- A* is time-sliced: 40 ms per tick, 5 s total; results `success | partial | timeout | noPath`.
  On `partial` the bot walks toward the best node while search continues.
- Path nodes carry `toBreak` and `toPlace` lists; dig and place costs enter A*. "Go there" can
  change the world, so a skill that moves needs a side-effect declaration.
- `Movements` is a clean policy object: `canDig`, `allowParkour`, `allow1by1towers`,
  `maxDropDown`, `blocksToAvoid`, exclusion areas as functions, cost multipliers.
- Goal interface: `heuristic(node)`, `isEnd(node)`, `hasChanged()`, `isValid()`, with
  `CompositeAny`, `CompositeAll`, `Invert`, `Follow`. Clean; reusable as is.
- Replanning triggers: block change near the path, chunk load next to a visited chunk, goal
  moved, dig/place error, and **3.5 s without reaching the next node**. Nothing detects a loop of
  repeated replans.
- `goto()` wraps the event soup in a promise with typed errors: `NoPath`, `Timeout`,
  `GoalChanged`, `PathStopped`.
- State is global and single (`stateGoal`, `digging`, `placing`): one pathing task per bot.
- `lib/human.js` (new): player-like locomotion calibrated on 48 recorded players (head-turn
  distributions, sprint delay, jump rate, pure-pursuit steering), reproducible from a seed.
  Its stated aim is to look human; relevant to Praxion's "feels like a natural player" goal.

### 3.4 Voyager (MIT, 2023)

Python orchestrator (curriculum, action, critic, skill agents; LangChain + OpenAI) and a Node
Express server running mineflayer (`/start`, `/step`). Each step: the LLM writes JS, Babel parses
it, the server `eval`s it, events (chat, error, save) and an observation come back, a critic LLM
judges success; up to 4 attempts.

- **Skill** = `{name, code, description}`; code is one `async function name(bot)`, description is
  an LLM summary of the code (≤6 sentences).
- **Storage:** `skills.json`, `skill/code/*.js`, Chroma (description embedding only). A skill with
  an existing name overwrites it; older files are kept as V2, V3 but only the newest is retrievable.
- **Retrieval:** query = task context + chat log summary, top-5 similarity. Success rate,
  preconditions and cost play no part.
- **Execution:** all skill code plus the control primitives is concatenated into one string and
  `eval`ed every step. Skills compose by calling each other by name; no dependency declaration,
  no versioning, silent shadowing on name clash.
- **Verification:** an LLM critic over a state summary (biome, inventory, equipment, health,
  nearby blocks, chests). Curriculum rule 8 avoids anything the inventory cannot show.
- **Hidden assists:** see Summary point 6. Also item-name aliases (`leather_cap` → `leather_helmet`)
  to patch names the LLM invents.
- **Control primitives (11):** `mineBlock`, `craftItem`, `smeltItem`, `placeItem`, `killMob`,
  `useChest`, `exploreUntil`, `shoot`, `waitForMobRemoved`, `givePlacedItemBack`, `craftHelper`.
  For the first 10 failures they write a chat message and return instead of throwing; the LLM
  reads it from the chat log. `mineBlock` never checks the count it was asked for.

### 3.5 Odyssey (MIT, IJCAI 2025, built on Voyager)

- Hand-written library: 41 primitive + 183 compositional skills in Voyager format. Quality varies:
  `craftIronDoor` adds one ingot per `mineIronOre` call without smelting or recounting. No tests.
- **MC-Comprehensive-Skill-Library** is the valuable part: one entry `obtainItem(bot, count, type)`
  solved recursively from tables: `pre_tool.json` (tool per block), `pre_item.json` (recipes,
  input/output counts, table needed), `pre_smelt.json`, `pre_collect.json` (mob/block drops),
  `func.json` (item → function), `map_name.json` (block → drop, equivalence classes such as all
  log types). 789 items across three dimensions in ~1,265 lines of JS.
- Lesson: "obtain X" tasks need no LLM. A dependency graph derivable from minecraft-data plus a
  deterministic solver covers them. Out of scope there: farming, husbandry, combat, building, portals.

### 3.6 mindcraft (MIT, very active)

One Node process. Each turn the LLM emits a `!command(args)` (41 actions + 8 queries) or uses
`!newAction` to write code (off by default: `allow_insecure_coding`).

- **Skills:** 38 hand-written functions in `library/skills.js` (2,093 lines) plus `world.js`
  queries, documented with JSDoc. No learning: generated code is written to
  `bots/<name>/action-code/N.js` and never added to the library.
- **Retrieval:** embedding of each doc's first two lines; word overlap without an embedding model.
  `placeBlock`, `wait`, `breakBlockAt` are always shown.
- **Result model:** functions return a boolean and write text through `log(bot, msg)`; for the
  action manager, success means "did not throw". The LLM receives the last 500 characters of output.
- **Code loop (`coder.js`):** up to 5 attempts; before running, checks that every called
  `skills.*`/`world.*` exists and runs ESLint; errors go back to the LLM. Runs in an SES
  `Compartment` exposing only `skills`, `world`, `Vec3`, `log`.
- **Cancellation:** the text `if(bot.interrupt_code) return;` is injected after every `;\n` of
  generated code. The action manager waits 10 s for a stop, then kills the process.
- **Loop detection:** more than 3 actions less than 20 ms apart cancels resume; more than 5 exits.
- **Modes (reflex layer):** a priority-ordered list ticked every frame: `self_preservation`,
  `unstuck`, `cowardice`, `self_defense`, `hunting`, `item_collecting`, `torch_placing`,
  `elbow_room`, `idle_staring`, `cheat`. The first active mode blocks the rest. A mode that
  interrupts an LLM action sends the LLM an automatic message ("your action X was interrupted,
  behaviour log: ..."); the interrupted action does not resume unless it registered a resume.
- **Stuck:** under 2 blocks of movement for 20 s (40 s on obsidian) → `moveAway(5)`; if that takes
  over 10 s, the process is killed.
- **`goToGoal`:** plan 1 s with a non-destructive policy (dig cost 10, never break glass), then a
  destructive one; while walking, open a random nearby door after 1.2 s without movement.
- **Cheat mode** is an explicit switch: placing becomes `/setblock`, travel becomes `/tp`.
- **Memory:** an LLM summary truncated to 500 characters, plus named places. No structured world memory.

### 3.7 altoclef (MIT, Java Fabric client mod on Baritone, ~34k lines, last commit 2023-12)

Goal: long-horizon autonomy up to a full speedrun. No LLM; entirely hand-written.

- **Task** = `onStart`, `onTick → Task | null`, `onStop(interruptTask)`, `isFinished`,
  `isEqual(other)`. Each tick `onTick` returns the subtask that should run now; if it is not
  `isEqual` to the current one, the old subtask stops and the new one starts. Lose the pickaxe,
  and next tick the tree switches to the "get a pickaxe" branch.
- `isFinished` reads the world: `ResourceTask.isFinished` = target count in inventory.
- `ITaskCanForce.shouldForce`: any task in the active subtree can refuse interruption (mid-air
  parkour, valuable item on the cursor). `interrupt` suspends (runs `onStop`, stays active);
  `stop` ends.
- Scoped settings: `getBehaviour().push()/pop()` (protected items, avoided blocks) for the task's lifetime.
- **Chains:** `TaskRunner` computes each active chain's `getPriority()` every tick and runs the
  highest. Typical values: world survival (escape lava, drowning) 100/60, MLG bucket 100/60,
  mob defense 65–70 (creeper: 50 + fuse × 50), food 55, user task 50.
- **Trackers:** blocks, entities, misc blocks, chunks, and a **container cache** of chests opened
  before. `ResourceTask` checks dropped items, then known chests, then mining.
- **Blacklisting:** counts failures per target and excludes it after N; resets the count when the
  bot gets closer or obtains a better tool.
- **Progress checkers:** `DistanceProgressChecker` (move at least d within T),
  `LinearProgressChecker` (metric grows by x within T), `ProgressCheckerRetry` (N attempts),
  `MovementProgressChecker` (distance while walking, dig progress while mining; mining counts as
  progress only once the block really is air; reset while eating).
- `TaskCatalogue` builds tasks from item names (the hand-written counterpart of Odyssey's solver).

### 3.8 Baritone (LGPL-3.0: ideas only)

- **Cost = expected ticks** per movement type (walk, jump, fall, dig, bridge, parkour).
- **Expectation-based timeout:** a movement taking `movementTimeoutTicks` (100) more than its cost
  estimate is cancelled.
- **Recheck before executing:** if a movement's cost rose by more than `maxCostIncrease` (10) since
  planning, cancel and replan. Off-path detection after 200 ticks away.
- **Segmented planning:** plan the next segment (4–5 s budget) while executing the current one,
  then splice; drop history older than 300 movements.
- **Processes** (Mine, Build, Farm, Follow, Explore, GetToBlock, Elytra, Backfill, InventoryPauser)
  expose `isActive()`, `priority()`, `onTick(calcFailed, isSafeToCancel) → PathingCommand`,
  `isTemporary()`, `onLostControl()`. Processes do not move the player; they return commands
  (`SET_GOAL_AND_PATH`, `REQUEST_PAUSE`, `CANCEL_AND_SET_GOAL`, `REVALIDATE_GOAL_AND_PATH`,
  `DEFER`, ...). Temporary processes pause permanent ones without taking control.
- **Cached world:** each chunk packed at 2 bits per block (air, water, avoid, solid; ~16 KiB per
  chunk), written to region files, plus a height map and positions of special blocks (chests,
  furnaces, portals, spawners, shulkers). Enables long-distance planning through unloaded chunks
  and "where was that chest" queries.

### 3.9 Craftonomous (MIT, TS, clean-room, 2026-08)

Not a bot: a body, senses and scoreboard exposed over MCP. `docs/PRIOR_ART.md` is a license-noted
survey of 52 open-source Minecraft agent projects and is worth reading in full.

- **Skill contract:** `{name, summary, description (including when not to use it), input: Zod,
  output: Zod, timeoutMs?, precondition?(ctx, input), run(ctx, input) → SkillResult}`.
- **SkillResult:** `{ok: true, value, durationMs}` or `{ok: false, kind, message, retryable, durationMs}`.
  Failure kinds: `timeout`, `precondition`, `unreachable`, `interrupted`, `world-changed`,
  `invalid-input`, `not-permitted`, `rate-limited`, `unknown` ("should shrink over time").
  Retryable: `timeout`, `unreachable`, `world-changed`, `rate-limited`.
- **SkillContext has no mineflayer handle:** skills see the world only through a gated, counted
  `WorldView`, act through an `ActuatorPort`, and get a `Clock` and an `AbortSignal`. An in-memory
  fake body lets the whole test suite run offline.
- No declared postcondition, but mining skills measure the inventory delta (so `iron_ore` →
  `raw_iron` is counted honestly).
- **Perception provenance:** every fact is `Observed<T>` tagged `proprioception`, `sight`,
  `hearing`, `memory` (with age), `inference`, `testimony` or `privileged`. Provenance combines
  pessimistically: anything derived from a privileged read stays privileged.
- **Profiles:** `fair-play` (64-block sight, occlusion enforced by a DDA voxel walk, 16-block
  hearing, 10-minute memory, privileged denied), `xray`, `omniscient`. A denied read throws;
  out of range returns `undefined` ("unknown", not "absent"). A ledger reports the privileged share.
- **Testimony** never becomes sight; at best it is marked "speaker could have known".
- **Reliability:** rank and gate on the Wilson score lower bound at 95% (1/1 ≈ 0.21, 5/5 ≈ 0.57,
  38/40 ≈ 0.84). Retire after ≥8 attempts below 0.25; retirement flags, never deletes. 24 h window
  with an 8-event floor; stats per context key, falling back to the global record when thin.
- **Reflex arbiter:** pure and synchronous `evaluate(world)`, priority-ordered; when a reflex fires
  it aborts the running skill's `AbortController`. Testable directly.

### 3.10 minecraft-agent-swarm (MIT, TS, 2026-09)

Local-LLM (Ollama) multi-bot system with hand-written TS skills, Voyager JS skills and runtime-
generated skills. Its value is operational lessons recorded in code comments.

- **Team-wide reliability and retirement:** "the team retried craftWoodenPickaxe 56 times with
  1 success" because precondition failures were excluded from broken-skill detection.
  Retire below 10% after 8 attempts.
- **The cost of retirement:** a `NEVER_RETIRE` list exists because `build_nether_portal` was
  retired at 1/11 and stranded the Nether strategy, and `wood_run` was retired at 0/9 while its
  answers were "no standing tree in sight". Without separating "skill broken", "world unsuitable"
  and "precondition unmet", reliability numbers make wrong decisions.
- **Generated-skill pipeline:** the LLM writes one async function against a narrow capability API
  (`observe`, `navigate`, `mine`, `craft`, `equip`, `consume`, `place`, `look`, `attack`, `wait`);
  no require/import/timers/infinite loops, ≤20 calls. Output goes to a candidate store and is used
  only once approved. Runs in a separate process with a seccomp policy, per-method quotas
  (navigate 4, mine 8, place 8, ...) and distance limits. Source note: `vm.createContext` is not a
  security sandbox.

### 3.11 Issue trackers (most-commented issues)

- **mineflayer (~2,100 issues):** breakage on every Minecraft release (1.17, 1.18, 1.19, 1.21
  support threads with 26–45 comments); non-vanilla server plugins break assumptions
  (`blockAt` returns null with a skin plugin); moving world state to prismarine-world still open (#334).
- **mineflayer-pathfinder (177):** stuck or halting (farmland #278, `GoalNear` never finishing
  near non-full blocks #205, freeze after `stop()` #261), terrain (snow #29, wheat #129, chests
  #242), random scaffolding failures (#296, open), slow decisions (#213), modular movements (#20, open).
- **Voyager (164):** mostly setup; behaviour: items not added to inventory (#40), "no chest at
  coordinates" (#56), small models fail (#157), mineflayer restarting (#99).
- **mindcraft (394):** mostly LLM provider setup; behaviour: "bot isn't doing what I say" (#46),
  spamming (#30), constant crashes (#741, open).

## 4. Skill definitions compared

| | Voyager | Odyssey | mindcraft | altoclef | Craftonomous | agent-swarm (generated) |
|---|---|---|---|---|---|---|
| Unit | One `async function(bot)` | Same | Hand-written JS function | `Task` class | `Skill<In,Out>` with Zod schemas | One async function on a narrow API |
| Author | LLM, kept on success | Human | Human (LLM code not kept) | Human | Human | LLM; candidate, then approved |
| Success test | LLM critic over a state summary | None (did not throw) | Did not throw | `isFinished` reads the world | Skill's own result, inventory delta | Did not throw |
| Failure channel | Chat message ×10, then exception | Same | `log()` text + boolean | Tree switches branch | Typed `FailureKind` + `retryable` | `{success, message}` |
| Cancellation | None | None | Injected flag checks | `stop` / `interrupt`, refusal allowed | `AbortSignal` | `AbortSignal` + process kill |
| Storage | JSON + JS + Chroma | JS + description files | Source code | Source code | Registry + reliability store | Candidate and approved stores |
| Retrieval | Description embedding, top-5 | Same | First-two-lines embedding | By name (`TaskCatalogue`) | Registry + reliability ranking | Reliability ranking |
| Precondition | None | None | Ad hoc inside | Branching in `onTick` | `precondition()` | None |
| Reliability | None | None | None | Per-target blacklist | Wilson bound, per context | Rate, retirement, exemptions |

No system lets a skill declare its expected effect (a postcondition or world delta) separately
from its code so that the framework checks it independently. Altoclef puts it inside the task
(`isFinished`); Craftonomous inside each skill's own measurement. This is an open gap for
Praxion's verification layer.

## 5. Candidates to take

| What | From | License | Proposal |
|---|---|---|---|
| Typed failure kinds + `retryable` | Craftonomous | MIT | Take; extend (e.g. separate "world unsuitable") |
| Wilson-bound reliability, per-context stats, retirement as a flag | Craftonomous | MIT | Take |
| Retirement exemptions; "world or skill?" lesson | agent-swarm | MIT | Idea |
| Perception provenance and profiles | Craftonomous | MIT | Idea, likely take |
| Body port + fake world for offline tests | Craftonomous | MIT | Take the structure |
| Reactive task tree (`onTick → subtask`, `isEqual`, world-reading `isFinished`) | altoclef | MIT | Idea; candidate execution model |
| Dynamic-priority chains, refusal of interruption | altoclef, Baritone | MIT / LGPL | Idea |
| Progress checkers, blacklist with reset conditions | altoclef | MIT | Take |
| Expectation-based timeouts (estimate + margin) | Baritone | LGPL | Idea |
| Goal interface and composites, time-sliced A* | mineflayer-pathfinder | MIT | Take or adapt |
| Vanilla physics simulation | prismarine-physics | MIT | Porting candidate |
| Tech-tree solver and tables | Odyssey | MIT | Idea; regenerate tables from minecraft-data |
| 2-bit chunk cache + special-block index | Baritone | LGPL | Idea |
| Container cache | altoclef | MIT | Idea |
| `syncWindow`, sequence acks, empirical server timings | mineflayer | MIT | Take; this is undocumented knowledge |
| Generated-skill pipeline: narrow API, candidate/approval, process isolation, quotas | agent-swarm | MIT | Idea |
| Static pre-checks on LLM code (functions exist, lint) | mindcraft | MIT | Take |
| Explicit cheat switch | mindcraft | MIT | Idea: if cheats exist, make them visible |

## 6. Patterns to avoid

- Measuring skill success as "the function did not throw".
- Reporting failures through chat text; inconsistent failure channels in one system.
- One global namespace of concatenated skill code; overwrite on name clash.
- Fixed-time stuck detection; teleporting out of trouble.
- Resuming an interrupted script as if the world had not changed.
- Automatic retirement without separating failure causes.
- Undeclared perception assumptions.
- One global action slot and global mutable state as the base for concurrency.

## 7. Not yet read in depth

- Craftonomous `src/planning/` (DAG, decomposer, knowledge graph) and `docs/ARCHITECTURE.md`.
- From Craftonomous's survey, not cloned: VillagerAgent (task DAG engine), Plan4MC (STRIPS-style
  skill schema), XENON (experience-corrected recipe graph), ADAM (causal tech tree), MineLand
  (48 concurrent bots, limited senses), hermescraft (fair-play perception), plancraft (offline
  symbolic crafting benchmark).
- Mineflayer world and entity tracking (`blocks.js`, `entities.js`).
- Protocol-level version differences (node-minecraft-protocol).

## Sources

mineflayer https://github.com/PrismarineJS/mineflayer · mineflayer-pathfinder https://github.com/PrismarineJS/mineflayer-pathfinder ·
prismarine-physics https://github.com/PrismarineJS/prismarine-physics · node-minecraft-protocol https://github.com/PrismarineJS/node-minecraft-protocol ·
Voyager https://github.com/MineDojo/Voyager · Odyssey https://github.com/zju-vipa/Odyssey ·
mindcraft https://github.com/kolbytn/mindcraft · altoclef https://github.com/gaucho-matrero/altoclef ·
Baritone https://github.com/cabaletta/baritone · Craftonomous https://github.com/ReverseZoom2151/Craftonomous ·
minecraft-agent-swarm https://github.com/JesseRWeigel/minecraft-agent-swarm
