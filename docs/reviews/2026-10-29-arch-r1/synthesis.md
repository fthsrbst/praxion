<!-- Adjudicated by Claude Opus 5.5 (workflow subagent), 2026-10-29. Not reviewed by the writer of architecture.md. -->

# Architecture review round 1: synthesis

## Method

- **Reviews.** There were 18 independent reviews of `docs/architecture.md` draft 1:
  - 13 from OpenCode models: Beam ×5, Iceman ×5, GLM-5.3-Flash, DeepSeek v4.1 Flash and Muse Spark 1.3.
  - 5 from Claude with web research: Opus ×1 and Sonnet ×4.
- **Clustering.** About 204 raw findings became 45 blocker/major clusters plus 61 minor items. Two adversarial verifiers (Iceman, Beam) then tried to refute each cluster.
- **Adjudication.** I checked every cluster against `architecture.md`, `decisions.md` and `sessions/02`–`15` with grep and full reads. I did not take verifier verdicts on trust:
  - Iceman cited sections that do not exist ("architecture §5.1", "D5") on C42.
  - Beam's proposed ingest-time line-of-sight filter would lose data. It is rejected under R1-M3.
- **External facts.** These were verified by the orchestrator on 2026-10-29: Minecraft 26.3 ships SDL instead of GLFW, the `rawMouseInput` option is gone and keys now use scancodes. Azalea's README says it supports 26.2.
- **One new finding.** R1-B5 comes from the adjudicator; no reviewer raised it.

## Verdict in one paragraph

The design direction holds. No reviewer found a reason to abandon the proxy-plus-OS-input body, the teleo-reactive skills or the tiered decision stack. The draft is not ready to drive the 2.7 experiments or any code, for three reasons:

1. **The input analysis under the riskiest bet is stale.** It describes GLFW, but 26.3 runs SDL3.
2. **The draft departs from decisions it claims to consolidate.** It reopens the proxy process boundary (D1) and leaves out more than ten decided behaviours.
3. **The contracts every later component stands on are unwritten.** That covers the body port, the actuator seam, three-valued `unknown` and per-action receipts.

There is also an unnoticed contradiction at the heart of the first demo. Typing a chat command, or using the interface's text box, takes control back by the decided take-over rule.

Eight blockers must close before 2.7 or the first code. Twelve majors must close before the first code of their component. Most fixes are short spec additions, not redesigns. Six items need a user decision because they touch agreed decisions.

## Blockers

| ID | Problem | Fix | Conflicts with decision? | Sources |
|---|---|---|---|---|
| R1-B1 | **Hands analysis targets the wrong input stack.** `sessions/02` §2.3 verifies GLFW `win32_window.c` (WM_INPUT, `lParam` scancodes, WM_KILLFOCUS). Minecraft 26.3 runs SDL3, `rawMouseInput` is removed and key bindings use SDL scancodes, so H1 ("bindings read from `options.txt`") and E5's "raw input on and off" axis describe code that no longer runs. SDL main special-cases `hDevice == NULL`, and injected `SendInput` events carry `hDevice = 0` (reported, unverified for the bundled SDL build). Relative injected mouse moves may therefore behave differently or not reach the camera. | Add a desk step before round 2: read the 26.3 `MouseHandler`, `KeyboardHandler`, `options.txt` key format and the bundled SDL Windows raw-input path. Rewrite §2.3 findings as SDL3. Drop the raw on/off axis from E5. Add explicit E5 checks: does an injected relative move turn the camera, does pointer speed or enhanced pointer precision change the gain, and is `hDevice = 0` handled. Keep the client-mod fallback as a *proposal* on a miss. | No. H1–H3 stand; only their evidence and E5 change. | C07 (PROTO-1, BC-7, WIN-6, WIN-7, PM-2, SAF-INPUT-2), WIN-10 |
| R1-B2 | **Decoder strategy and version baseline are unpinned.** Azalea is a client framework. Its 26.3 support is on a branch while the README states 26.2; it is reported to need nightly Rust. Mojang reports give ids and registries, not packet field layouts (§4 implies otherwise). A passive decoder must track Handshake→Login→Configuration→Play, Play→Configuration re-entry, and Known Packs (where Registry Data may omit entries, already noted in `sessions/02`). `decisions.md` has no dated version bullet; 26.4 snapshots are shipping. | Before E2: (a) record a version-baseline decision (26.3, protocol 777, Java 25, date); (b) pin the client, server jar, Azalea commit and toolchain date in one manifest; (c) require E2 to decode a **stock-client** capture covering Configuration, Known Packs and one Configuration re-entry; (d) state in §4 that field layouts come from the decoder library, ids and registries from Mojang reports; (e) name an own minimal decoder for the consumed packet subset as the first fallback, MCProtocolLib as a test oracle. | Yes: pinning versus 1.1's rolling "newest stable" rule. User question Q4. | C04 (PM-1, BC-1–4, PROTO-2–4, PROTO-6, SAF-PROXY-3, PLAN-8), C05 (HOL-10, PM-8, CONS-4) |
| R1-B3 | **Proxy process boundary reopened.** §2 and risk #3 mark "forwarder as own process or isolated thread inside the body" as [O]. D1 decided a separate proxy process so that "the game is unaffected" by decoder failure. A thread shares the body's process, which runs a planner and an interpreter over LLM-written rule lists, so a body panic would drop the user's live connection. The draft also moves the decoder from the proxy (D1's wording) into the body without saying so. | Remove the [O] and the thread fallback. State as a requirement that no body or decoder crash may close the client–server connection. Recommended topology: a minimal forwarder process (no decoder, no game logic) that tees bytes to the body over loopback, with the decoder and world model in the body. E1 and E3 measure exactly this topology. On a budget miss, propose moving the decoder into the forwarder process, never the forwarder into the body. | Restores D1. Where the decoder lives clarifies D1 → Q3. | C01 (HOL-1, CONS-1, BC-5, TIME-COST-004) |
| R1-B4 | **The 2.7 plan cannot validate the timing budget.** E1–E14 measure isolated properties. Nothing measures the integrated loop (packet → decode → world model → reflex/arbiter → hands → server ack) under load. Round 1 latency on macOS does not represent Windows scheduling with the game, the test server and recording running. Chunk-burst decode is not separated from world-model update. There is no server setup spec (see R1-M6). On a miss, the user gets "stop" with no prepared options. | Add the changes listed under "Changes to the 2.7 experiment plan": macOS limited to correctness, Windows co-load runs with p99/p99.9, a new integrated-tick experiment E15, a server checklist, and a fallback shortlist per threshold that is **proposed to the user**, not taken automatically. | Partly. The stop rule stays; fallbacks are only proposals, so there is no conflict. | C44 (PLAN-1, PLAN-2, TESTING-2), C06 (PROTO-5, PROTO-9, HOL-9) |
| R1-B5 | **No command input path works while Praxion plays (new).** The take-over rule says any key press takes control back. To give the demo's "verified chat command" the user presses the chat key (`T` or `/`), which is a physical key, so Praxion stops before the command exists. The interface text box cannot help either: `WS_EX_NOACTIVATE` windows cannot receive typing without activation, and activation makes the game lose focus, which H3 turns into a stop. The demo script in decision 1.5 (hand-over → chat command → take-over) is not executable as specified. | Define a **command gesture**. A physical press of the bound chat or command key while Praxion plays: (1) releases all non-danger-held keys (session 4's exception still applies); (2) waits while the chat screen is open; (3) if the next serverbound chat is a prefixed command, Praxion takes it as the command and keeps control; (4) otherwise it is a normal take-over. Treat clicking into the interface the same way. Test it in E12. | Yes: refines the 2.4 take-over sensitivity rule and the "never take focus" invariant. Q1, Q2. | Adjudicator; C33 (NF-1, CONS-2, NF-5) |
| R1-B6 | **Safety-critical control depends on the brain and the network.** "Stop" and "cancel always work", but session 9 routes all text through Jev, and the §2 diagram attaches the indicator and interface to the brain. With the brain or the network down, stop, status and carry on do nothing and the indicator freezes. Outage behaviour is also undefined: 2.5 says stop safely and stand still; session 8 says run a safe fallback goal after 3–5 s, and that goal is never defined. Session 7 requires that "the demo must survive a model outage", yet standing still at night is not survival. | (a) The body parses a fixed list of control verbs (stop, cancel, status, carry on, hand-over) with a local grammar before any brain call; Jev maps only the remaining commands. (b) The indicator subscribes to the body, not the brain. (c) Add one per-call-class deadline and fallback table to §3. (d) Define the outage state machine: finish the current primitive to a safe point, run a hand-written shelter or retreat core skill, then hold with reflexes on. | (a), (b), (c): no (session 9's Jev mapping is a minor pick). (d) amends 2.5 "stands still". Q5. | C11 (HOL-4, HOST-3, PM-3, HOST-7, TIME-COST-002/003, SAF-BRAINSTOP-9, SAF-SECRETS-6), C12 (deadline part) |
| R1-B7 | **The load-bearing contracts are unwritten.** The body port appears in `architecture.md` only as an arrow. `sessions/02` §2.5 has its three message kinds, but no ordering, resync after a brain restart, back-pressure, version handshake or generation-increment rule. Inside the body there is no actuator seam. Hands is a leaf behind planner, movement and primitives, yet the level-2 protocol body, the practice room and the client-mod fallback all need a different actuator, and the protocol body a different perception source. `unknown` is a [P] pick that no interface carries. | Before the first code, write a one-page **v0 port spec**: `hello{port_version, caps}`, a `seq` on every observation, snapshot-plus-delta resync, bounded per-key coalescing queues with a `dropped(n)` marker, the body-owned generation increment (owner change, take-over, new user command, reflex preemption), and a typed `Known(v) / Unknown(reason)` in every query answer. Also write the **actuator frame** contract: a per-tick record of held controls, look delta or target and clicks (Mineflayer `controlState`), with SendInput as the only v1 implementation and a headless packet actuator for level 2. Transport stays a first-code choice. | No. §2.5 defers only format and transport, as a minor pick. | C42 (HOL-6, PLAN-3, PLAN-6), C02 (HOL-2, COMP-1), C18 (port part) |
| R1-B8 | **No dependency-ordered first slice.** §10 lays out every directory, including brain, interface, `skills/candidates/` and RAG. No document defines the smallest end-to-end slice that proves the first night. Engineering principle 3 ("never do the last step first") then has no concrete gate. Reviewers misread the scope: the first demo is already the first night, not the dragon. | Add a "first slice" subsection. **Demo-0**: the protocol test body plus the hand-written rule-list core, reflexes, referee and fake world survive a scripted night on fresh seeds, with no SendInput and no LLM. **Demo-1**: the same core through the real client and SendInput, plus the R1-B5 command and take-over flow. First code is gated on Demo-0 only. Trim §10 to the directories Demo-0 needs; add the others when their demo needs them. | No. The capability target and the 18/20 demo gate stay. | C38 (PM-4, SC-1, PLAN-4, PLAN-5, PLAN-7) |

## Major

| ID | Problem | Fix | Conflicts with decision? | Sources |
|---|---|---|---|---|
| R1-M1 | **Receipts cannot verify movement and cannot reject stale acknowledgements.** The client is authoritative for position, so move and look never get "the server's word". The `unrefuted` status exists in session 4 but nothing consumes it. A global evidence ladder can mark a dig confirmed at its start acknowledgement. Matching OS input to the packets it produced is untested. | Make evidence contracts per action and generated as data: dig → block air; place → state; pickup → inventory delta; move and look → `unrefuted` (no correction for N ticks); slot click → client packet plus no correction. Add a receipt epoch rule (receipt tick > intention start tick). Make input-to-packet attribution part of E9. | Movement as `unrefuted` touches "success needs the server's word". Q7. | C22 (ALG-3, COMP-1, SKILL-5, TESTING-4) |
| R1-M2 | **Rule-list interpreter semantics are undefined.** Guards are boolean, so `unknown` silently becomes false. Nothing defines what happens when no rule is true, how rules switch mid-primitive, cleanup on halt, or `until` (yet session 7's arbiter requires "no `until` guard active"). Admission checks regression but not completeness. | Specify three-valued guards (Kleene AND/OR). A rule with an `unknown` guard is ineligible and offers an `observe(x)` subgoal. No eligible rule → `failed(no_rule)`, which wakes the brain. In-flight discrete actions block until their receipt or a timeout. Define `until` as a commitment guard. Specify it as a truth table plus a state diagram. Defer action diffing and `on_halt` hooks to first code. | No. | C18 (ALG-1, HOL-5, MEM-008), C27 (SKL-5, ALG-2) |
| R1-M3 | **The perception algorithm is unspecified, and §5's wording is wrong.** A 64-block sphere holds about 1.1 M cells; precomputing visibility is not feasible within the budget. "The world model keeps only what a player could see" would also lose data: a chunk is sent once, so a block hidden at load time but exposed later by digging would never be known. | Store everything the decoder receives (internal, never exposed). Enforce visibility at the **query API**: deny by default, bounded line-of-sight ray tests per candidate, cached per (eye chunk, target), with an explicit ray budget. Log denied access attempts, not denied facts. Fix §5's wording. A transparent-material table and an entity-tracking bound are decided at first code. Rejected: Beam's ingest-time filter. | No. The policy is unchanged; only the mechanism is specified. | C17 (ALG-4, PM-5, SAF-PERCEPT-8), C43 (perception part) |
| R1-M4 | **Persistent state has several writers and no owner.** The ledger, place and container memory, skill stats and reports are written from two processes. §4 puts memory in Python "read through the body" [P] while remembered facts originate in the body. Chunk-load invalidation and container refresh are unspecified. | The body's append-only event log is the single source of truth. Memory, ledger progress, skill stats and reports are projections that can be rebuilt from it; the brain writes proposals only. Containers are LIVE only while their window is open, otherwise SEEN with an age. No identity is assumed across break plus place. The stage-E chunk snapshot (Unload Chunk with an allowlist) moves from `research/` into §8, still behind its trigger. | No. The staged memory is kept. | C19 (ALG-12, HOL-7, HOL-8, MEM-001–004, 006, 009), C21 (COMP-7, MEM-007) |
| R1-M5 | **The take-over sensor can fail silently.** Low-level hooks are removed silently after a timeout. Remote-control tools and other injectors may look physical or injected inconsistently. The watchdog does not wait on the body process and does not track mouse buttons. A reflex taking ownership can drop danger-held keys. Hand-over fails after about 2 s if a key is still held. | Tag every injected event with a per-run nonce in `dwExtraInfo`; anything without it is the user. Add a periodic probe on an unbound key that proves the hook is alive. The watchdog waits on the body process handle and replays a shared held-key table that includes mouse buttons. Danger-held keys can be revoked by no tier and are released last. A pending hand-over waits for key release with no 2 s abort. All of this is tested in E12. | No (minor picks). Remote tools → Q8. | C08 (WIN-1, WIN-2, WIN-5, BC-8, ALG-9, SAF-TAKEOVER-1), C34 (NF-4), C09 (WIN-3) |
| R1-M6 | **The dedicated server is not transparent singleplayer.** There is no Esc pause, and the world keeps ticking on any stop, focus loss or wait for "carry on". Because input needs focus, the character is defenceless during those times. `white-list` reportedly defaults to true in 26.3 (Nodecraft; verify). Offline-mode UUIDs differ from online ones, which only matters for imported worlds. | Write a first-class "dedicated-server constraints" note. Add the server checklist (R1-B4). Add a pause-state heuristic: no client movement packets for N ms while active → safe stop, never inject into a menu. Record that "safe stop" cannot protect the character without focus, and that decision 1.4 ("death is acceptable") covers this. | No. "A local server counts as singleplayer" stands. Q6. | C10 (PROTO-7, BC-6, BC-11) |
| R1-M7 | **The primitive contract cannot support the guard compiler.** Game data yields inventory arithmetic only. Spatial and contextual preconditions (reach, support, free space, light, line of sight) are not in the data. Effects can be conditional or probabilistic. §4 says "the LLM writes rule lists" while session 5 says "steps and a goal". The decided obtain-X solver (sessions 9 and 10) is missing from the architecture. | One versioned contract record per primitive: data-derived effects (produces, consumes, changes, reveals, duration, tool tier, p) plus a hand-declared precondition block, cancellation class, required evidence and cost. Run a conformance test of declared against measured deltas in the fake world. Fix the wording: "a step program compiled to a rule list". Add the recipe and tech-tree solver to §6 and §4. | No. | C26 (SKL-4, ALG-10, SKILL-2), C41 (ALG-11), C28 (COMP-2, COMP-3) |
| R1-M8 | **The admission evidence path and statistics are unsound.** The practice room cannot run on the real body (SendInput needs the foreground on a dedicated PC), so it must run on the headless protocol body, whose fidelity is unknown. A k=3 McNemar test cannot reject at α=0.05 (3 discordant pairs give p=0.25), and the Wilson 95% lower bound for 3/3 is 0.44. Failures are never turned into edits. | State that admission evidence comes from the protocol body and gate it on a fidelity check (see 2.7). Define k=3 as a smoke test: 3/3 on the skill's own fixture, no regression on pooled fixtures, at least one held-out fixture from a different seed or biome, and no world-specific literals. Statistical confidence comes from probation (Wilson, at least 8 uses, as decided). Add a bounded repair loop: typed failure kind plus trace → writer, at most 3 revisions per goal. Add an optional suspected-primitive field to failures. | No. k=3 and test-server-only probation are kept. Q10. | C03 (HOL-3), C23 (SKL-1, SKILL-1, SKL-2, ALG-13, COMP-10, SKL-9, SKL-10, PM-7, SC-2, TIME-COST-005, SKILL-4/6/7, TESTING-5), C24 (SKL-6), C25 (SKL-3) |
| R1-M9 | **Cost runaway has no ceiling.** "Warn, never cut" plus event-driven calls allows a skill-fail → rewrite → fail loop overnight. Rate limits are per cause only. Test runs already hard-stop; play does not. | No-conflict part: cap repair attempts per goal (R1-M8), add a global LLM calls-per-minute limit and a consecutive-failure breaker that degrades to code-only, log dollars and calls per rung, and recommend a provider-side spend limit. Conflict part: one user-set hard stop that pauses autonomous LLM work while reflexes and the core keep running. | Yes, the hard stop conflicts with "never cut automatically". Q9. | C13 (HOST-2, PM-10, HOST-10) |
| R1-M10 | **The planner contract is incomplete.** There is no time budget per segment, no partial-result statuses, and no planning state for hotbar, best tool and the protected set. Azalea's pathfinder reportedly has no place, bridge or pillar moves and assumes instant camera moves (verify in the session 6 spike). | Add a planner-contract subsection: planning state, a time budget, output statuses (success / partial / timeout / noPath), `safe_to_expose` and bridge-support preconditions, and camera-then-move as a two-phase primitive. Reuse only Azalea's A* core and cost constants; write place, bridge and pillar moves ourselves. The replan triggers from session 6 are kept. | No. | C30 (ALG-5, ALG-6, ALG-7, COMP-8) |
| R1-M11 | **Safety rules lack enforcement points.** "Only the user's player is obeyed" names no mechanism. The ruleset has no storage, integrity check or ownership. | The body drops chat from any sender other than the user's offline-mode UUID (taken from the login) before parsing. The ruleset is a body-owned read-only file whose hash is checked on load. The brain may only propose changes, which the user confirms. The remote-brain port authenticates with a pre-shared key (fixes "body holds no keys"). | No. | C43 (SAF-CHAT-4, SAF-RULESET-5) |
| R1-M12 | **The draft is not traceable to `decisions.md` and mis-tags items.** Absent decided items include: user projects first, death acceptable, the 30 s announcement for big risky steps, resume-then-choose after an empty hand-over, the 150 ms reaction floor, reply language and tone, the wrong-click slot check, the `.env` fallback, the lesson trigger, the five 1.6 measures, and hearing plus the chunk cache (session 3 picks). Jev command mapping is tagged [D] although it is a minor pick. The language split rests on the unconfirmed "~10 ms" rule while `decisions.md` says "exact boundary decided in 2.5". | Add a decisions → section traceability table, one row per bullet, with gaps listed. Retag Jev mapping as [P]. Reword the `decisions.md` bullet to "boundary shape decided in 2.5; wire format and the ~10 ms split rule still proposed". Add one footnote in §4. Reword the 1.4 "proposal:" clause. | No. These are corrections. | C39 (CONS-3), C40 (CONS-5), C20 (COMP-6) |

## Minor and dropped

**Minor (fix in the draft-2 edit; no separate decision):**

- C15 + C16: state that the heartbeat's "did anything change?" check is code (a hash or diff over briefing facts), not a call class. Add a state hash to stamped directives. Keep the existing wake list and Jev hysteresis; do not import souta-lab's epoch set.
- C12 (rest): give each model role an explicit latency budget and reasoning effort. Cap the plan-early lead time. Measure first-token latency per role.
- C14: record Jev vendor risk (early access, no SLA, 30–60 day deprecation) and pin the Jev version. Treat `/v1/systemone` as a port with an LLM structured-choice implementation. Name one acceptance bar (accuracy and calibration on logged held-out decisions).
- C29: the session 10 pick already requires a computable completion condition. Place the validator in the brain before a directive is sent.
- C31: express every arbiter quantity in expected ticks. `loss_on_interrupt` is the running primitive's cancellation class plus its remaining ticks.
- C32: keep the RAG (a user decision). Keep it off the Demo-1 critical path behind a flag. Make the per-passage licence field mandatory, with a build check that fails on an NC passage in any distributable target.
- C36: add a "needs you" cue (a sound plus a taskbar flash via `FlashWindowEx`, no focus change) for high-risk questions only, and a pending-question state in the indicator.
- C37 + C45: add a test-level capability matrix to §9. Session 13 already defines naturalness metrics (KS / energy distance, AUC ≤ 0.6, paired forced choice).
- Remaining minor IDs (HOL-11/12, WIN-4/8/9/11/12/13, PROTO-8/10/11/12, HOST-8/9/11, SKL-7/8/11/12, COMP-9/11/12, PM-11–14, BC-9/10/12, SC-5–12, NF-7–12, ALG-14/15, CONS-6–11, TIME-COST-008, SKILL-8, TESTING-8–10, SAF-LOGS-10, PLAN-9) go to `backlog.md` under "Architecture review round 1 minor items".

**Dropped (these recommendations, not the clusters' kept parts):**

- **C35 auto-resume after take-over.** Contradicts decision 2.4. An empty hand-over already resumes the suspended task, and R1-B5's command gesture removes the common case.
- **C38 "re-scope away from the dragon" and "flawless vs 18/20".** Misreads: the first demo is already the first night, and the demo video is one of the measured runs.
- **C32 "drop RAG", C14 "Jev off in Demo-1", C29 "hard-off free curriculum".** Each reverses an explicit user choice without new evidence. Flags are allowed; removal is not.
- **C45 fixed N≥30.** Contradicts "targets set after the first baseline".
- **C37 second Windows box.** Unnecessary: the proxy and decoder are portable and level 2 runs from captures.
- **C02 "cancellation classes move onto the actuator frame".** Conflicts with session 4. Cancellation stays on actions; the frame only carries held state.
- **C23 "probation on the real body".** Contradicts "never first tried in the user's world".
- **C17 Beam fix (ingest-time line-of-sight filter).** Technically wrong; see R1-M3.

## Ideas worth adopting

1. **Capture corpus first, with a differential decode oracle.** Record stock-client 26.3 sessions before any decoder work. Compare the decoder per packet against the game's own codecs (a small Java harness) or MCProtocolLib. Every replay and CI test reuses the corpus. (cl-sonnet-protocol, oc-iceman-testing)
2. **Version-pin manifest.** One file pins the client, server jar, protocol number, Azalea commit, Rust toolchain date and a hash of the generated data. CI refuses mismatches; fixtures, skills and logs carry the manifest id. (cl-sonnet-protocol, oc-glm-consistency)
3. **Input tape with a nonce.** Log every injected event with its QPC time and `dwExtraInfo` nonce, aligned to proxy packets. This serves E9, input attribution (R1-M1), stuck-key forensics and take-over ownership. (cl-sonnet-input)
4. **Tick-synchronous input scheduler.** Use serverbound per-tick packets seen by the proxy as the clock for holds and clicks, instead of open-loop 50 ms timers. This directly targets E7 and E8. (cl-sonnet-input)
5. **Environment doctor at start.** Read-only checks of integrity levels, Sticky/Filter/Toggle Keys, pointer speed and precision, DPI, monitors, focus and sleep, each refused with a clear message. Add `SetThreadExecutionState` while active. Praxion never changes the settings. (cl-sonnet-input)
6. **One generated contract file per primitive.** Effects, preconditions, required evidence, cancellation class and cost in one place, feeding the guard compiler, the receipt checker and the test matrix. This implements R1-M7. (oc-deepseek-algorithms)
7. **Shadow mode while the user plays.** Run reflexes and skills on the live perception stream, log the actuator frames Praxion would have sent, and compare them with what the user did. This gives free real-body evidence without input conflicts. (cl-opus-holistic)
8. **Death-to-fixture factory.** Turn every death in the event log into a refereed fixture automatically, with counterfactual replay of a changed reflex. This automates session 7's "every death becomes a cell". (oc-iceman-testing, cl-opus-holistic)
9. **Replay harness for Jev versus a cheap LLM versus rules.** Score all three on logged decision points from the event log. This is the measurable entry bar session 8 already demands. (cl-sonnet-models)
10. **Hidden, rotating hold-out fixtures for the gate.** Keep fixtures and thresholds out of the skill writer's reach and rotate the held-out set, so the gate cannot be overfit. (cl-sonnet-skills)
11. **Planner-only control arm in the learning test.** A no-LLM planner over the same effect data separates what the LLM adds from what the data adds. (cl-sonnet-skills)
12. **Prompt-prefix caching.** A stable system prompt plus the primitive catalog form the cacheable prefix, with the briefing after it. Combined with the code-level heartbeat diff, idle cost drops toward zero. (cl-sonnet-models)

## Decisions needed from the user

1. **Q1 Chat commands during play.** Recommendation: pressing the bound chat or command key is a *command gesture* (R1-B5). Praxion releases non-danger keys and waits; a prefixed command sent from that chat both commands and keeps Praxion in control; anything else is a normal take-over. Main alternative: every key, including `T`, is a take-over, and a chat command implies a fresh hand-over.
2. **Q2 Interface text input.** Recommendation: keep "Praxion's windows never take focus" for the indicator. Clicking into the interface's command box is allowed and counts as a take-over, which H3 already implies; the box is for use while Praxion is not playing. Main alternative: a read-only interface with chat as the only text channel.
3. **Q3 Proxy topology.** Recommendation: make "no body or decoder crash may drop my game connection" a hard requirement. A minimal forwarder runs as its own process; the decoder and world model live in the body. Main alternative: D1 read literally, with the decoder inside the proxy process sending decoded events to the body (one less hop for raw bytes, but more code in the process that must never crash).
4. **Q4 Version pin.** Recommendation: pin Minecraft 26.3, the server jar, the Azalea commit and the toolchain until Demo-1 passes, and record it as a decision. Upgrades need your sign-off. Main alternative: follow decision 1.1's rolling "newest stable at coding start" rule.
5. **Q5 Safety without the brain.** Recommendation:
   - the body parses stop, cancel, status, carry on and hand-over locally;
   - the indicator is fed by the body;
   - on brain silence the body finishes the current primitive, runs a hand-written shelter or retreat skill, then holds with reflexes on.

   Main alternative: keep 2.5 as written (stand still with reflexes), and accept that a night outage in the open may kill the character.
6. **Q6 Dedicated-server trade-offs.** Recommendation: accept the loss of Esc-pause, and accept that a stopped Praxion cannot defend the character while the game is unfocused (covered by "death is acceptable"). Add a pause heuristic and the server checklist. Main alternative: reopen the client-mod route for an integrated server, which conflicts with "avoid mods".
7. **Q7 Movement verification.** Recommendation: move and look count as verified when `unrefuted` (no server correction for N ticks); world changes keep level 2 and effects keep level 3. Main alternative: restrict the verified command suite to world changes and inventory effects only.
8. **Q8 Remote control of the Windows PC.** Recommendation: during play the PC is operated only physically. Remote tools (Parsec, Sunshine, AnyDesk) are unsupported for take-over, and the nonce tag (idea 3) still guards ownership. Main alternative: support remote play, which adds a remote-tool take-over test to E12 and may require nonce-only detection.
9. **Q9 Cost brake.** Recommendation: keep "warn" as the first stage, and add a per-goal repair cap, a global call-rate breaker that degrades to code-only, and one high hard stop you set that pauses autonomous LLM work (reflexes and the core keep running). Main alternative: warn only, as decided, relying on a provider-side spend limit.
10. **Q10 Admission evidence.** Recommendation: practice-room evidence comes from the headless protocol body after it passes a fidelity check. k=3 is a smoke test: 3/3 on its own fixture, no regression, and one held-out fixture. Statistical confidence comes from Wilson bounds in probation on the test server. Main alternative: run practice rooms on the real client only while you are not playing, which is slower but avoids the fidelity question.

## Proposed changes to docs/architecture.md

- **Header:** add a traceability table (decision bullet → section), list its gaps, and fix the tags R1-M12 lists.
- **§1 What Praxion is:**
  - cite the version-baseline decision (26.3, protocol 777, Java 25);
  - add the decided behaviours that are missing: user projects first, death acceptable, the five 1.6 measures, reply language and tone, the 150 ms reaction floor with its lethal-danger exemption;
  - add the "first slice" subsection (Demo-0 / Demo-1).
- **§2 System overview:**
  - forwarder as its own process [D, D1], with the [O] and the thread fallback removed;
  - the decoder location per Q3;
  - the indicator wired to the body;
  - local control-verb parsing in the body;
  - the actuator frame shown as the seam above Hands (SendInput for real play, a packet actuator for level 2).
- **§3 Time scales:**
  - a per-call-class deadline and fallback table;
  - per-role model latency budgets and reasoning effort;
  - "all arbiter quantities in expected ticks";
  - the brain-outage state machine (Q5).
- **§4 Components:**
  - Decoder row: ids and registries from Mojang reports, field layouts from the pinned decoder.
  - Hands row: SDL3 (not GLFW), the actuator frame, the wrong-click slot check.
  - Interface row: split into requirements [D] and technology [O], plus the Q2 rule.
  - New rows: version-pin manifest and recipe/tech-tree solver.
  - Memory row: owner per R1-M4.
  - Language footnote: rests on the proposed ~10 ms split rule.
- **§5 Data flows:**
  - Perception: store all, expose via a deny-by-default query API with a ray budget; `unknown` is a typed value.
  - Action: per-action evidence contracts, `unrefuted` for move and look (Q7), the receipt epoch rule.
  - Commands: local control verbs, the command gesture (Q1), the UUID sender filter.
  - Brain loop: the wake list aligned with session 8 plus the session 6, 7 and 10 causes; the heartbeat as a code diff; a state hash on directives.
  - Hand-over: wait for key release with no 2 s abort; the empty hand-over resume-then-choose branch [D].
- **§6 Skills:**
  - three-valued guards, the `failed(no_rule)` outcome, `until` semantics;
  - the primitive contract record with hand-declared preconditions;
  - "step program compiled to a rule list";
  - the obtain-X solver;
  - admission evidence path and statistics per Q10;
  - the bounded repair loop.
- **§7 Safety:**
  - ruleset as a body-owned read-only file with a hash check;
  - pre-shared-key authentication on the remote port (replacing "body holds no keys");
  - continuous detection of blocked input (no client packet echo within N ticks → `input_blocked`);
  - remote-tool policy (Q8).
- **§8 Memory:**
  - the event log as the single source of truth, with projections rebuilt from it;
  - the container read rule;
  - no identity assumed across break plus place;
  - the stage-E Unload Chunk snapshot behind its trigger;
  - hearing as a sense with provenance `hearing` (session 3 pick).
- **§9 Testing:**
  - a test-level capability matrix (level 2 excludes input timing, focus and menus);
  - a protocol-body fidelity gate;
  - the E15 integrated test;
  - the R1-B6 outage state machine in the chaos test.
- **§10 Layout:** trim to the Demo-0 directories; the rest "added when their demo needs them".
- **§11 Risks:**
  - add: SDL3 input path, command gesture versus take-over, dedicated-server focus and pause, cost runaway, Jev vendor risk, protocol-body fidelity;
  - add a "fail path (proposed to the user)" column;
  - risk 10 is split and owned by R1-B7, R1-M2 and R1-M4.

## Changes to the 2.7 experiment plan

- **New round 0 (desk, before anything else):**
  - write the version-pin manifest;
  - read the 26.3 `MouseHandler`, `KeyboardHandler` and `options.txt` key format plus the bundled SDL Windows input path;
  - record a stock-client capture corpus (Status ping, Login, full Configuration with Known Packs, one Play→Configuration re-entry, 10+ min of Play).
- **Server checklist (all rounds):**
  - `online-mode=false`, `server-ip=127.0.0.1`, `white-list=false` (verify the 26.3 default), `enforce-secure-profile=false`;
  - a decision on `network-compression-threshold=-1`;
  - the EULA accepted by the user only.
- **E1 and E3:** macOS round 1 judges correctness only. The latency thresholds are judged on the Windows PC with the game, the test server and the recorder running. Report p99 and p99.9; measure decode and world-model update separately; report chunk bursts apart. Measure the R1-B3 topology. Use `TCP_NODELAY` and a bounded queue that never blocks forwarding.
- **E2:** decode the stock-client corpus, not an Azalea bot, at the pinned commit. Pass = zero decode errors **and** per-packet agreement with the oracle (idea 1). Include Known Packs and Configuration re-entry.
- **E5:** drop "raw input on/off". Add a pointer speed / enhanced-precision matrix and an explicit check that an injected relative move turns the camera under SDL3 (`hDevice = 0`).
- **E7 and E8:** close the loop on serverbound per-tick packets (idea 4) instead of open-loop holds.
- **E9:** extend from latency to attribution: which injected event produced which serverbound packet (feeds R1-M1).
- **E10:** compare `SetCursorPos` with absolute `SendInput` for menu positioning.
- **E11:** borderless is reportedly the 26.3 default; measure focus loss under SDL3 in all three window modes.
- **E12:** add the following, with the E12 pass criteria fixed before the run:
  - the `dwExtraInfo` nonce check;
  - the hook-liveness probe;
  - the watchdog waiting on the process handle and releasing mouse buttons;
  - danger-held keys released last;
  - the Q1 command gesture (chat key opens, the prefixed command is accepted, Praxion keeps control);
  - a remote-tool take-over case if Q8 is answered yes.
- **New E15, integrated tick (Windows, round 4):** packet → decode → world model → reflex/arbiter → hands → server ack, under a chunk burst plus a combat fixture. Pass: end-to-end p99 of a scripted reflex (lava step-back) within the 50 ms reflex budget, with no forwarding delay.
- **New E16, protocol-body fidelity probe (after round 4):** run the same scripted movement and dig sequence on the real body and the headless body and compare traces. It gates R1-M8 and the level-2 claims.
- **E14:** pass criteria are written before the runs (stated outcome: chosen decoder plus the reason).
- **On a miss (stop rule unchanged):** the report carries a pre-written fallback shortlist for the user to choose from:
  - E2 → own minimal decoder, MCProtocolLib as the oracle;
  - E3 → decoder moved into the forwarder process;
  - E5–E8 → a client-mod spike;
  - E12 → nonce-only ownership.
