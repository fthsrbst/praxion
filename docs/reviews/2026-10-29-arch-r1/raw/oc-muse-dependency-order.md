<!-- Reviewer: Muse Spark 1.3 Contributor via OpenCode opencode v2.1.2. Lens: muse-dependency-order. Raw, unverified output. -->

Adversarial plan review — checking dependency order and the first demo path.
I've mapped the experiment plan and demo targets — now digging into the ordering gaps.
Direct searches are unavailable — I'll verify the remaining details from the local docs.
### Findings

- **ID**: PLAN-1
- **Severity**: blocker
- **Where**: `docs/architecture.md` §11; `docs/sessions/02-body-connection.md` §2.7
- **Claim**: E1–E14 do not test the riskiest assumption behind the first demo, and have no usable kill criteria.
- **Evidence**: E1–E13 measure isolated micro-properties (proxy p99 <1 ms, aim within 1° in 150 ms, 95/100 one-tick holds, keys released in 100 ms). Nothing measures an integrated tick: decoder → world model → reflex/arbiter → hands → server ack under load (chunk burst + combat + take-over armed). E14 is explicitly a "report", not a pass/fail. The decided stop rule is "a missed threshold stops the work and goes to the user; no automatic route change", i.e. stall-by-default with no pre-agreed fallback branch (client mod, own minimal decoder, MCProtocolLib, freeze version at 26.2).
- **Recommendation**: Replace the stop rule with a decision table before any spike: for each of E1/E2/E3/E5/E12 state threshold → action (e.g. E2 fails twice within 2 days → spike own decoder for needed packets only, timeboxed 3 days → then user picks mod vs. freeze). Make E14 falsifiable: "adopt Azalea iff 10-min 26.3 session decodes with 0 errors AND p99 state-update <5 ms on the Windows PC; else fallback", with the comparison rubric committed before runs.
- **Confidence**: high

- **ID**: PLAN-2
- **Severity**: blocker
- **Where**: `docs/sessions/02-body-connection.md` §2.7 "Order"; `docs/architecture.md` §4
- **Claim**: Round 1 "cheap, on macOS first" measures the wrong machine and creates a circular dependency.
- **Evidence**: Timing budgets (E1 p99 <1 ms, E3 p99 <5 ms, E13 p99 <2 ms) are Windows-loop properties (`timeBeginPeriod`, waitable-timer jitter, game tick, SendInput path), but Round 1 runs on macOS against captures. The body is decided Windows-only. E5–E9 (hands) need a working proxy decoder for "proxy-verified" scoring, so Rounds 2–4 depend on Round 1 passing — yet Round 1's numbers do not transfer. Result: a pass on macOS proves little, a fail wastes the Windows trip.
- **Recommendation**: Run E1–E3 on the Windows PC from the start (loopback, real client + real server); keep macOS only for decoder-correctness unit tests against captures. Define E4/E9 "measure only" with a pre-registered consumer (e.g. E9 feeds the settle-window table in session 4), or drop them from the gate.
- **Confidence**: high

- **ID**: PLAN-3
- **Severity**: major
- **Where**: `docs/architecture.md` §§5,10,12; `docs/sessions/02-body-connection.md` §2.5
- **Claim**: The body port — the interface every level of testing depends on — is deferred to "chosen at the first code", so the E1–E14 spikes are throwaway by construction.
- **Evidence**: §2.5: "Message format and transport are chosen when the first code is written; they are an implementation detail." Architecture §12 confirms "message formats and syntax (chosen at the first code)". But session 14 requires "the fake body is the same port as the real one, so one suite runs against all levels". Spikes written before the contract cannot grow into that harness.
- **Recommendation**: Freeze a minimal v0 port contract *before* E1–E14: three message kinds with schemas (observation delta, command {id, owner token}, receipt {pending/settled + evidence level}), plus a recorded-replay file format. Write spikes against it. Anything that cannot be expressed in v0 is out of the first demo.
- **Confidence**: high

- **ID**: PLAN-4
- **Severity**: blocker
- **Where**: `docs/architecture.md` §§4,6,8; `docs/decisions.md` (2026-10-29: Jev, curriculum, RAG); `backlog.md` ("Run the session 2.7 experiments")
- **Claim**: The plan builds late-needed systems early and early-needed primitives late; there is no defined smallest vertical slice for "survive the first night".
- **Evidence**: First night needs roughly: move/look, dig, place/tower, eat, sleep-or-hide, 3–5 reflexes (creeper, skeleton, zombie, fall, drowning), take-over + stop, one command ("stop/status/come"), receipts level 2/3, referee on a test server. It does not need: Jev middle tier, LLM skill writer + compiler guard derivation, free curriculum vs. ladder arm, wiki RAG, lessons, movement-style layer, interface live view/memory viewer, project ledger. Yet all of the latter are designed, decided, and queued before or alongside first code, while the primitive catalog, settle windows, and per-action cancellation classes are "measured during implementation".
- **Recommendation**: Define Demo-0 / Demo-1 explicitly. Demo-0 (headless, no hands, no LLM): protocol test body + hand-written TR core + fake world + referee survives scripted night on N fresh seeds. Demo-1: same core through real client + SendInput. Gate first code on Demo-0 only. Move Jev wiring, skill writer, curriculum comparison, RAG build, lessons, and style layer to "after Demo-1, only on measured trigger".
- **Confidence**: high

- **ID**: PLAN-5
- **Severity**: major
- **Where**: `docs/architecture.md` §5 "Action and verification"; `docs/sessions/04-action-verification.md`; `docs/sessions/07-reflexes-arbiter.md` §"Demo gate"
- **Claim**: The first-demo acceptance predicate is contradictory and unfalsifiable as written.
- **Evidence**: Decisions say the first demo "must be flawless" (session 1.5). Session 7/14 say the gate is "at least 18 of 20 fresh seeds without death" plus "100% on reflex cells over 10 instances each". Architecture §1 repeats "survives the first night alone" with no seed count, no command suite subset, no starvation-gap SLO, no cost/latency bound for the demo run. "Flawless" (1/1 video) and "18/20 with CIs" cannot both be the gate.
- **Recommendation**: Write a one-page Demo-1 contract now: seed policy (e.g. 20 fresh seeds, fixed client settings), must-pass command subset (stop, status, carry-on + 2 gathering commands from session 9 day-one list), death budget, max starvation gap, max take-over latency, referee verdict rules. Label the video as one of the 20. Delete "flawless".
- **Confidence**: high

- **ID**: PLAN-6
- **Severity**: major
- **Where**: `docs/sessions/03-perception-world-model.md` "Left open"; `docs/sessions/12-memory-rag.md`; `docs/architecture.md` §11 #10
- **Claim**: Three load-bearing contracts needed by the first line of body code are explicitly left open with no spike to close them.
- **Evidence**: Object identity (moved chest vs. break+place), TTLs per fact kind, and the briefing/observation query contract are deferred to "session 8 and 12" / "first code and measurement". Architecture risk #10 admits "format of the rule lists; where the guard compiler runs; who owns the memory file" is resolved only at "first code". The planner may only use "blocks the world model knows" (session 6) and receipts depend on re-reading "the truth" (session 4) — both undefined until these close.
- **Recommendation**: Add a pre-code spike E0 (timeboxed, ≤1 week): freeze `unknown` semantics, three TTLs (blocks/entities/containers), the five body queries the first-night core needs, and the TR text syntax subset (≤5 rule shapes). First code starts only when E0 is merged as a schema, not prose.
- **Confidence**: high

- **ID**: PLAN-7
- **Severity**: major
- **Where**: `docs/architecture.md` §§4,10; `docs/sessions/15-infrastructure.md`
- **Claim**: §10 proposes the full-repo scaffolding up front, violating the repo's own "build in dependency order, no speculative scaffolding" rule.
- **Evidence**: `AGENTS.md`: "Build in dependency order… No speculative scaffolding, no 'for later' abstractions." Architecture §10 lays out `body/ brain/ interface/ harness/ skills/ candidates/` with layering tooling before Demo-0 exists, and §4 commits to Jev+LLM clients, curriculum, RAG, and interface window as components. None is needed for E1–E14 or Demo-0. This front-loads build-system, lint, and inter-process work before the decoder/hand-over risks are retired.
- **Recommendation**: Start with `spikes/` → `harness/` (fake world + referee + replay) + one `body/` crate (forwarder+decoder+world subset) + hand-written core skills. Add `brain/`, `interface/`, `skills/candidates/`, RAG, and ledger only when their Demo needs them. Enforce the layering rule from Demo-0, not from an empty workspace.
- **Confidence**: medium

- **ID**: PLAN-8
- **Severity**: major
- **Where**: `docs/sessions/02-body-connection.md` §2.2; `docs/architecture.md` §11 #2; `backlog.md` "Session 6 spike and numbers"
- **Claim**: The Azalea dependency — the foundation of decoder, world, physics, and planner reuse — rests on a version claim that already looks stale, with no version-freeze plan.
- **Evidence**: Session notes (2026-10-23/29) claim Azalea has a `26.3` branch (pushed 2026-10-16) and MCProtocolLib merged 26.3 on 2026-10-14. A live fetch of `github.com/azalea-rs/azalea` today states "Currently supported Minecraft version: 26.2" on main — unverified whether the 26.3 branch still exists or decodes cleanly. Architecture bets decoder + `astar/moves/costs` + `azalea-physics` lift-out on this, while noting the planner "may be bound to Bevy ECS too tightly" (risk #11) with only "a small spike with the first code" to find out. If 26.4 ships before Demo-1, the plan has no pin/freeze rule.
- **Recommendation**: E2 must test the exact pinned commit hash of Azalea against 26.3 on the Windows PC and record it; add a freeze rule (demo pins client+server+decoder commits; version bumps are explicit user decisions). Timebox the planner lift-out spike to days, with the fallback (own A* over known blocks following Baritone expected-tick costs) as the default if the lift fails — do not let planner reuse block Demo-0.
- **Confidence**: medium (main-branch README verified by fetch; branch state unverified — web search unavailable in this session)

- **ID**: PLAN-9
- **Severity**: minor
- **Where**: `docs/sessions/02-body-connection.md` §2.7 note; `backlog.md` "Run the session 2.7 experiments"; `docs/sessions/15-infrastructure.md`
- **Claim**: The process plan serializes everything through one Windows PC and one user decision, with stale `spikes/` hygiene.
- **Evidence**: §2.7 says "experiment code lives in `spikes/`, untracked, until the license is chosen" — but session 15 already chose Apache-2.0, and `backlog.md` still repeats the stale rule. Hands/focus/take-over rounds all require the physical PC, yet there is no parallelization (decoder work on macOS vs. hands on PC), no CI/nightly slot, no timebox per round. Combined with PLAN-1's stall-on-miss rule, one missed threshold blocks all lanes.
- **Recommendation**: Update the rule now that the licence is chosen (allow committing `spikes/` harness helpers or delete them on merge). Split lanes: lane A (decoder/world/fake-world, anywhere) and lane B (hands/focus/take-over, Windows PC), each timeboxed (e.g. 1 week per round) with a pre-agreed fallback. Record results into `docs/sessions/02-body-connection.md` as the backlog already requires.
- **Confidence**: high

- **ID**: PLAN-10
- **Severity**: major
- **Where**: `docs/sessions/10-autonomy.md`; `docs/sessions/11-skill-learning.md`; `docs/sessions/14-testing.md`
- **Claim**: The "free curriculum" and "wiki RAG" choices (both taken against recommendation) multiply the first-demo test surface without a demo-scoped off-switch.
- **Evidence**: Session 10 adopts a free Voyager-style curriculum with only a logging mitigation ("a goal that keeps Praxion away from the next rung is logged"); session 12 adopts wiki RAG with an open licence conflict (CC BY-NC-SA vs. open core) and an unchecked dump-terms precondition. Both import open-ended failure modes (unverifiable goals, retrieval failures) into the critical path, with measurement deferred to "the learning test" / "real runs". The demo needs neither.
- **Recommendation**: Hard-off both for Demo-1: ladder-spine goals only, game data from Mojang reports only, RAG disabled. Keep the free-curriculum arm and RAG retrieval test as post-Demo-1 experiments behind flags. Resolve the wiki dump terms + licence-vs-open-core conflict before any index is built (it is currently "to be checked before building" with no owner/date).
- **Confidence**: high

### Ideas (brainstorm)

1. **Demo-0 before Demo-1**: survive a scripted night headless (protocol body + referee, no SendInput, no LLM) — retires verification/arbiter logic without the Windows bottleneck. Why: fastest falsification of the TR core.
2. **Body-port-first contract**: freeze observation/command/receipt schemas and a replay file as v0 this week — makes every spike reusable as a test fixture. Why: turns throwaway spikes into the permanent harness.
3. **Kill-board with pre-agreed fallbacks**: one table mapping each E-threshold miss to a timeboxed fallback and owner. Why: replaces stall-on-miss with motion.
4. **Pin-and-freeze rule**: demo pins client, server, and decoder commit hashes; version bumps need user sign-off. Why: stops the 26.x treadmill from invalidating E2 mid-demo.
5. **Hand-written first-night core as the permanent trust tier**: 8–12 reviewed TR skills + 5 reflexes, LLM writes nothing for Demo-1. Why: shrinks the demo's blast radius to code that can be refereed.
6. **Referee-first development**: build the harness referee (server-fact verdicts, fault injection for dropped acks/stale inventory) before the body features it judges. Why: every later claim becomes checkable on day one.
7. **Defer Jev/RAG/curriculum behind feature flags with cost kill-switches**: all hosted calls default off in Demo-1 config; chaos test (cut providers mid-night) becomes part of the demo gate. Why: proves "silent brain → safe stop" instead of asserting it.

### Questions for the user

1. Is Demo-1 allowed to run with Jev/LLM/RAG/free-curriculum hard-off (ladder-spine + hand-written core only), or must the first night use the full brain?
2. On an E-threshold miss, may the agent switch to the pre-agreed fallback (own minimal decoder / version freeze / client-mod spike) without asking, or must every miss wait for you?
3. Which gate defines "first demo success": the flawless video (1 run) or 18/20 fresh seeds with confidence intervals — pick one?
4. Do you accept pinning client + server + decoder commits for the demo (no 26.4 upgrades mid-stream)?
5. May `spikes/` helpers be committed now that Apache-2.0 is chosen, or must experiment code stay untracked?

FINAL_REPORT Top 3: the E1–E14 plan tests isolated parts with no integrated-tick experiment and a stall-on-miss rule instead of pre-agreed fallbacks; the body-port contract and world-model TTLs/identity needed by the first line of code are deferred to "first code", guaranteeing throwaway spikes; and the plan front-loads Jev, skill-writer, curriculum, and RAG — none needed to survive the first night — while leaving the minimal vertical slice (hand-written TR core + referee + Demo-0 headless) undefined.
