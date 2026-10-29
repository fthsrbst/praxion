<!-- Reviewer: Claude Opus 5.5 (Claude Code workflow subagent). Lens: opus-holistic. Raw, unverified output. -->

### Findings

**HOL-1. Severity: blocker. Where: architecture.md §2 and §11 #3, compared with decisions.md (2026-10-29, "Proxy: a separate process") and sessions/02 D1**
- **Claim:** The draft reopens a decided safety property. §2 says it is "[O]" whether the forwarder is its own OS process or "an isolated thread inside the body". D1's reason for a separate process is that "the game is unaffected" when the decoder breaks. The body holds about 14 components, including a Rust A* planner and an interpreter running LLM-authored rule lists. If the forwarder lives in that process, any panic or abort in the body drops the user's live TCP connection, and the user is kicked from their own world. A thread is not isolated from a process abort.
- **Evidence:** decisions.md: "Proxy: a separate process that forwards bytes untouched and decodes a copy." architecture §2: "with the isolated thread as the fallback".
- **Recommendation:** Keep the forwarder as its own minimal process with no decoder and no game logic. It tees bytes over a loopback socket or shared-memory ring to the body. Measure the tee cost in E1/E3. If the budget fails, move the decoder into the forwarder, not the forwarder into the body. Remove the [O].
- **Confidence:** high

**HOL-2. Severity: blocker. Where: architecture §2, §4 "Hands", §11 #1; decisions 1.2 ("two bodies")**
- **Claim:** The biggest unknown (synthetic input at endgame timing, E5–E12) has no defined seam inside the body. The client-mod fallback, the level-2 "protocol test body" and the practice room all need a different actuator, and the protocol body may also need a different perception source. Right now "Hands" is a leaf component behind planner, movement style and primitives. If E5–E12 fail, or level-2 needs packets instead of keys, nothing says what is swapped. The body port (brain↔body) is the wrong seam for this: these swaps happen inside the body.
- **Evidence:** §4 lists "Hands | SendInput..." with no interface. Session 14 says level 2 is a "headless client on a local test server", which by construction sends packets, not keys. Mineflayer, which decisions.md names as the reference model, separates `bot.setControlState(control, state)` / `bot.look(yaw, pitch)` from the transport (https://github.com/PrismarineJS/mineflayer/blob/master/docs/api.md).
- **Recommendation:** Define an **actuator contract** now: per-tick `ControlFrame {held: set<Control>, look_delta | look_target, clicks: [Use|Attack|Slot(n)], sneak/sprint mode}`. It has three implementations: SendInput, a headless packet client, and a later mod. Everything above it (primitives, planner, movement style, reflexes) produces only ControlFrames. Take-over and cancellation classes act on ControlFrames. This is the single highest-value interface to write before first code.
- **Confidence:** high

**HOL-3. Severity: blocker. Where: architecture §6, §10 "practice room"; session 11; session 14 level 2; decisions 2026-10-23 "PC is dedicated to the game"**
- **Claim:** Skill admission cannot run on the real body while Praxion plays, and the substitute body differs in exactly the part that fails. `SendInput` injects into the single global input stream, so it goes to whatever window has focus. A second real client on the test server cannot be driven while the user's client is in front. The practice room therefore runs on the headless body. That body must simulate client physics itself (the real client normally does that, per §2.1), so movement, timing and menu clicks differ from production. Skills are admitted on evidence from body B and run on body A. The draft also never says how "a copy of the world" is taken from a running server without corrupting it.
- **Evidence:**
  - SendInput docs: events are inserted "into the keyboard or mouse input stream" and are subject to UIPI (https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput).
  - sessions/02 §2.1: "The client is authoritative for movement: it runs physics".
  - The fidelity question is listed only as "Risk 4", with no consequence for admission.
  - Making a copy on demand needs a flush. Whether that counts under "no operator commands" when it is issued on the user's server console is unverified against the ruleset.
- **Recommendation:** State explicitly that admission evidence comes from the headless body. Add a "probation on the real body" stage: the first N real uses are watched and receipts are compared with the practice-room effect signature. Schedule practice rooms only while the user is not playing, or on a second machine. Specify the world-copy mechanism. Pick between copying from the last autosave and flushing through a harness-only console that the ruleset whitelists.
- **Confidence:** high

**HOL-4. Severity: blocker. Where: architecture §5 "Commands", §7; session 9; session 2.5 "body stops safely and tells the user"**
- **Claim:** The safety-critical paths run through the hosted, fallible tier.
  1. "stop" and "cancel always work", but chat is decoded in the body, sent to the brain and mapped "by Jev to a closed schema". With the network or the brain down, typing `p: stop` does nothing.
  2. On brain silence the body must "tell the user", but the only user-facing channel in §2 (the interface window) hangs off the brain. In-game chat needs typing, which takes 1–3 s, needs focus and posts as the user.
- **Evidence:**
  - Diagram §2: "Interface window ◄── BRAIN".
  - session 9: "Jev ... maps text to a verb plus typed arguments".
  - session 2.5: "the body stops safely and tells the user".
- **Recommendation:** Parse a fixed list of control verbs (stop, cancel, status, carry on, hand-over) with a local grammar in the body, never through Jev. Connect the indicator/interface to the body: either the body serves it, or the interface subscribes to both ports. The "who controls, is the brain alive" state must render with the brain dead.
- **Confidence:** high

**HOL-5. Severity: major. Where: architecture §5 "brain loop", §6, §11 #10; sessions 3, 5**
- **Claim:** The rule-list (TR) format is the central brain→body artifact, and its semantics are undefined in a way that conflicts with a decided rule. Perception makes `unknown` a first-class value. TR picks "the highest true rule" each tick. Nothing says how a guard over `unknown` evaluates: false, which scans down to lower rules, or a blocking "explore" outcome. With line-of-sight-only perception, most resource guards (`table_in_reach`, `nearest(oak)`) will often be unknown. The choice changes behaviour, resume semantics and the guard compiler. "Format chosen at first code" leaves the hardest part, the condition language and its logic, to whoever writes the first code.
- **Evidence:**
  - session 3: "Unknown is a value, not 'absent'".
  - session 5: "each tick the highest true rule fires".
  - §12: "Message formats and syntax (chosen at the first code)".
- **Recommendation:** Before first code, specify a minimal condition language: its predicates, three-valued (Kleene) logic, and the rule "unknown in a guard → the rule is not eligible, and an `observe(x)` sub-goal is offered". Specify where the guard compiler runs. Recommendation: in Rust, sharing the primitive effect tables, with Python calling it through the port. The main alternative is a Python compiler fed a generated effects file. That duplicates the semantics in two languages.
- **Confidence:** high

**HOL-6. Severity: major. Where: architecture §2.5 body port (only in sessions/02), §5; no port spec in architecture.md**
- **Claim:** Phase 0 commits to "the same port serves the real body, fake world and replays", yet the body port is not in the architecture doc at all. Five things are missing:
  - message catalog, ordering guarantees and query semantics (snapshot vs live);
  - back-pressure (event bursts during chunk loading or combat while Python is in a 20 s LLM call);
  - resync after a brain restart (how the brain rebuilds suspended tasks and directive generations);
  - version negotiation;
  - what increments the `generation` that stamped directives depend on.

  Without these, the fake world and the real body will drift apart, and replay equivalence is unprovable.
- **Evidence:** sessions/02 §2.5: "Message format and transport are chosen when the first code is written". architecture §5 cites "generation" without defining it.
- **Recommendation:** Write a one-page port spec covering:
  - a `hello{port_version, body_caps}` handshake;
  - a monotonically increasing `seq` on every observation;
  - snapshot-plus-delta resync, where the brain asks `snapshot()` and then receives deltas after `seq`;
  - bounded queues with coalescing per key and a `dropped(n)` marker;
  - `generation` incremented by the body on owner change, take-over, a new user command or a reflex preemption.

  Pick the transport now (for example length-prefixed messages over a loopback TCP or named pipe) so the fake body is built against the real framing.
- **Confidence:** high

**HOL-7. Severity: major. Where: architecture §4 "Memory and briefing [P]", §8, §6 skills in git, §10 `skills/`; session 10 ledger; session 14 event log**
- **Claim:** Persistent state has at least six writers across two processes with no ownership or crash-consistency model:
  - body event log (Rust);
  - per-world JSONL memory (Python, "read through the body"?);
  - project ledger (brain);
  - chunk cache (body);
  - skills git repo with an automatic rollback pointer (brain or harness);
  - candidates dir (skill writer process);
  - user-play recordings.

  Remembered facts originate in the body (provenance, line-of-sight re-check) but the draft places memory in Python. Skills live under the product source repo's `skills/`, so runtime-learned, per-user, possibly per-world artifacts mix with Apache-licensed source and dev git history.
- **Evidence:** §4 "Python (files read through the body) [P]"; §10 "skills/ ... candidates/ (writer-only)"; §6 "Versions live in git with automatic rollback".
- **Recommendation:** Make the body's append-only event log the single source of truth. Memory, ledger progress, skill reliability statistics and run reports are rebuildable projections of it, which matches the "progress recomputed from the world" decisions. The brain writes only proposals: candidates, lesson text, ledger intents. Move learned skills to a per-installation data directory with its own store, separate from the source repo. The main alternative is brain-owned memory with a body event subscription: simpler at first, but it creates two truths after a crash.
- **Confidence:** medium-high

**HOL-8. Severity: major. Where: architecture §5 "Hand-over and take-over", §4 Arbiter; sessions 2.4, 7, 9, 10**
- **Claim:** Task-intent state is split across processes with no owner. The arbiter (body) has "user's command" and "own projects" tiers. Suspension of the old command (session 9), the frozen task waiting for "carry on" (2.4), the curriculum and the project ledger (brain) all hold overlapping pieces of "what we are doing". When the brain restarts, the body knows the running skill but not the suspended command stack. When the body restarts, the brain's ledger lists goals the body never heard of.
- **Evidence:**
  - session 9: "A new command suspends the old one and the old one resumes afterwards".
  - session 2.4: "the program is frozen".
  - session 10: "Ledger ... on disk". Neither names a process.
- **Recommendation:** Define one intent stack, owned by the body and persisted in the event log. Entries are `{intent_id, tier, skill_ref, args, state: running|suspended|frozen_for_carry_on|done}`. The brain pushes and pops through the port. The ledger keeps only long-horizon goal trees, which point to intent ids.
- **Confidence:** medium

**HOL-9. Severity: major. Where: architecture §2 (one Windows PC), §11; E13 in sessions/02 §2.7**
- **Claim:** The runtime budget only accounts for the game running. During play the same PC runs:
  - the Minecraft client and the user's vanilla server;
  - the forwarder and the body;
  - the Python brain;
  - a local embedding model and wiki RAG (Qwen3-Embedding, session 12);
  - possibly a test server plus a headless body for admission;
  - the interface window;
  - OBS recordings for the blind test.

  E13 gates "jitter of the 50 ms loop with the game running", p99 < 2 ms, but not under this co-load. No experiment measures the 5 ms decode budget with the embedder or a second server busy.
- **Evidence:** sessions/02 E13; session 12 embeddings are local; session 11 practice room on a test server.
- **Recommendation:** Add E13b: jitter and decode p99 under worst-case co-load (embedding batch, test server, OBS). Specify process priorities and affinity, and move embedding and RAG to on-demand or off-play times. Alternatively, put the practice room and RAG on a second machine. This is already allowed, since 2.5 says the brain may be remote.
- **Confidence:** medium (the co-load effect itself is unmeasured)

**HOL-10. Severity: major. Where: architecture §4 Decoder, §11 #2; decisions 1.1 "version-specific code in one layer"**
- **Claim:** Version coupling is unmanaged. The client, server jar, decoder, Mojang data reports, primitive effect tables, rule-list guards, regression fixtures (world copies) and the event-log schema are all tied to one game version. No component is the "version pack" that decision 1.1 requires. Azalea's README currently names 26.2 as its supported version, not 26.3, so the decoder already lags the target. A launcher auto-update or a user picking "latest release" silently breaks perception.
- **Evidence:** https://github.com/azalea-rs/azalea README: supported version 26.2 (fetched 2026-10-29). There is a `26.3` branch per sessions/02 that I did not verify. Decisions 1.1: "Version-specific code must stay in one layer".
- **Recommendation:**
  - Add a `version_pack` layer to §4/§10: decoder adapter, generated data tables, effect tables, and the protocol number.
  - Have the forwarder check the handshake protocol number and refuse on mismatch with a clear message.
  - Pin the launcher profile.
  - Stamp every fixture, skill and log record with the pack id.
- **Confidence:** high

**HOL-11. Severity: minor. Where: architecture §3 vs session 13**
- **Claim:** The "<50 ms reflexes" row conflicts with the decided ~150 ms human reaction floor, which allows only lethal reflexes to be faster. Which reflexes count as "lethal" is undefined. That category decides both the naturalness test and the survival rate.
- **Evidence:** session 13 decision: "reaction floor about 150 ms ... lethal-danger reflexes may be faster".
- **Recommendation:** Tag each reflex in the coverage matrix with `latency_class: human_floor | lethal_fast` and report the share of fast reactions in the blind-test analysis.
- **Confidence:** high

**HOL-12. Severity: minor. Where: architecture §6 vs session 5/11**
- **Claim:** Composition depth and size rules conflict and are scattered:
  - "depth capped at 2" (s11);
  - "more than 30 rules" rejected (s5);
  - "skills of about 2 to 5 steps" (s5).

  The recipe/tech-tree solver "compiled into a rule-list skill" (s9/s10) will produce deeper chains, for example diamond pickaxe → iron → furnace → stone → wood. That route bypasses or breaks the depth cap, and its admission path is unstated: generated skills versus LLM skills.
- **Recommendation:** Classify solver-generated programs as framework-generated. They are exempt from the LLM-skill gate but must come only from verified tables, with a stated depth limit.
- **Confidence:** medium

### Structural changes (top 3, each with its main alternative)
1. **An actuator contract (ControlFrame) inside the body** (HOL-2, HOL-3). This isolates the largest unknown, makes the test body and the mod fallback swappable, and lets cancellation classes and take-over act on one type. *Alternative:* abstract at the primitive level, with each body implementing `dig()` and `place()`. It is easier for the headless body but duplicates every primitive per body.
2. **Event log as the single source of truth, with an intent stack owned by the body** (HOL-7, HOL-8, HOL-6 resync). Memory, ledger progress, skill statistics and reports become projections. Replay and the brain restart both come for free. *Alternative:* brain-owned memory and ledger with subscriptions to body events. It is simpler for the first demo, but crash recovery needs reconciliation logic.
3. **Three runtime processes with a narrow safety core: forwarder, body, brain, plus a watchdog.** Stop/cancel/status and the indicator are served by the body (HOL-1, HOL-4). The brain becomes truly optional for safety and visibility. *Alternative:* one Rust process holding everything except the brain. It has less IPC, but a body crash then disconnects the game, which breaks D1.

### Ideas
- **Write the ControlFrame and port spec first, then build the fake world against them.** All level-1 tests then sit on the production interfaces, so they don't need rewriting later.
- **Counterfactual replay:** the event log plus a deterministic interpreter lets you re-run a death with a changed reflex or skill offline. This could beat every Minecraft agent on "learns from each death" with zero tokens.
- **Shadow mode while the user plays:** the body runs skills and reflexes "in its head" on the real perception stream and logs the frames it would have sent against what the user did. This gives free real-body evaluation, a naturalness reference and admission evidence without input conflicts.
- **Proxy-certified claims as a public differentiator:** every reported success links to receipt ids checked by an independent referee. No existing agent (Voyager, Mindcraft and the agents surveyed in docs/research/survey/01) publishes verdicts its own self-report cannot influence.
- **A player-bound, real-client, fair-perception leaderboard** (line of sight, F3-only): define the benchmark others must meet, not only the agent.
- **"Explain why" from the log:** each action traces to rule → guard values with provenance → directive stamp. A complete causal answer to "why did you do that?" is rare among LLM agents.
- **Typed `unknown`-driven curiosity:** treat unknown guard values as exploration goals, so exploration falls out of the perception policy instead of being prompted.
- **Version-pack regression across Minecraft releases:** re-run admitted skills when a new pack arrives. Learned skills survive game updates, which agents tied to Mineflayer typically do not.

### Questions for the user
1. Can admission (the practice room) run only while you are not playing, or on a second machine? Otherwise it competes with your game for input and CPU (HOL-3, HOL-9).
2. Should the body, rather than the Python brain, own the indicator and the stop/cancel/status commands, so they work with the brain or the network down (HOL-4)?
3. Do you accept "a forwarder crash or body crash must never kick me from my world" as a hard requirement? It rules out the forwarder-as-thread fallback (HOL-1).
4. Should learned skills live outside the product repository, in a per-installation data folder (HOL-7)?
5. Is it acceptable for skills to be admitted on headless-body evidence, followed by a watched probation on the real body (HOL-3)?

### Sources
- https://github.com/azalea-rs/azalea (README: supported version 26.2, crate list, MIT)
- https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput (global input stream, UIPI)
- https://github.com/PrismarineJS/mineflayer/blob/master/docs/api.md (`setControlState`, `look` signatures; table of contents only, the full text was truncated)
- Repo files read: `/Users/fatih/praxion/docs/architecture.md`, `/Users/fatih/praxion/docs/decisions.md`, `/Users/fatih/praxion/docs/sessions/02-body-connection.md` through `15-infrastructure.md` (except 13), `/Users/fatih/praxion/docs/research/decisions-to-code.md`, `/Users/fatih/praxion/backlog.md`