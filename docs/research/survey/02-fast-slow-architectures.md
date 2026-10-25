# Dual-system and asynchronous fast/slow control

Research date: 2026-10-25. Markers: [V] verified by an independent check, [C] corrected after checking, [D] documented or vendor claim, [U] unverifiable.

Scope: how real-time systems keep acting while a slow model thinks, and what interface connects the tiers. Session numbers refer to `docs/discussion-plan.md`.

## In brief

- **Every surveyed real-time system runs two or more tiers at once; the fastest never waits.** Helix: S2 at 7-9 Hz over S1 at 200 Hz, S0 at 1 kHz in Helix 02 (vendor). DPT-Agent: rules checked every 0.25 s, rewritten by an LLM every ~6-19 s. GR00T is "dual-system" in name only: one synchronous call per 40-step chunk.
- **Pausing the world hides the problem.** TickingCollab (Minecraft, ~20 s API delay): success at most 0.65 paused, at most 0.15 in real time. In Real-Time Reasoning Gym a pure planner falls from 0.92 to 0.05 under time pressure; AgileThinker, reactive model and planner in parallel, keeps 0.58. The binding rule is **runway**: each slow output must keep the agent busy longer than the next call's p95 latency.
- **Best interface for hosted LLMs over exact proxy state:** a teleo-reactive guarded rule program (as in DPT-Agent) plus plan-carried monitors (DoReMi, Code-as-Monitor), both checked by code every tick. Learned-latent interfaces (Helix, GR00T) need joint training.
- **Staleness:** provenance stamps, guards on current state, validation at acceptance. Never inject updates into a reasoning model (late updates cost up to 60%); validate, or cancel and restart, which still bills partial tokens.
- **Robotics has an open async contract (RTC, LeRobot).** Praxion needs its policy parts (generation counter, age bounds, HOLD then grace then fault, provenance), not its 50 Hz action queue.
- **Think sparsely, on events.** Game-TARS on Minecraft: 63% sparse vs 45% every step. Polling adds half a period to expected reaction time.
- **Speculation is lossless only for reversible actions,** and almost nothing in Minecraft is. Speculate on plans and queries, never on game input.
- **Proposed stack for session 8**, fastest tier first:

  | Tier | Rate | Role |
  |---|---|---|
  | Hands | render rate | game input |
  | Executor, reflexes, monitors | 20 Hz tick, local code | carry out programs, react |
  | Jev | when the deadline exceeds its p99 (~1 s or more) | bounded choices |
  | LLM | runway longer than its p95 latency | write guarded programs |

Dropped after checking: nothing was refuted. Corrected: openpi has no RTC port (RLinf has a third-party one); LeRobot faults, not just rejects, on a too-old source observation at acceptance; VLASH numbers are v1; the SIA method is SI-SFT; AgileThinker authors span Georgia Tech, Stanford, Tsinghua, SJTU; OpenHA is MIT. The architect removed RTC-weight blending for camera and path: that control is closed-loop, so retarget from current velocity.

## Key mechanisms

### 1. Guarded rule programs with plan-carried monitors [V]

**Teleo-reactive (TR) program** (Nilsson 1994): an ordered list of `condition -> action` rules; every tick, the highest rule whose condition holds fires. **Regression property:** each rule's action normally achieves the condition of a higher rule. The program skips ahead when lucky and falls back when unlucky, without re-planning. Triangle tables (Shakey, 1972) are the plan-suffix form: execution resumes from the latest suffix whose kernel (needed conditions) holds.

**DPT-Agent** is the LLM-written version, tested in real-time Overcooked:

```
LLM, async: every 25 steps (~6.25 s), at once on a human message
  | writes
  v
[(precondition, macro_action)]
   e.g. ("lambda s: s['objects'][('Beef','Well-cooked')] < 2", prepare(...))
  | checked every 0.25 s
  v
FSM --(no rule applies)--> default rules
  | macro action
  v
A* executor --> atomic moves
```

Score 46.63 vs ReAct 15.48, mean over 11-17 LLMs (text and table disagree). DPT-Agent `eval()`s the LLM's Python; Praxion would compile a side-effect-free DSL instead.

**DoReMi:** each plan step carries `Constraint: <must hold while the step runs>`. A checker tests all active constraints every 0.2 s; on a violation it aborts the skill, appends the violation to the prompt and re-plans. Humanoid moving boxes that randomly drop: 94% vs 46%. **Code-as-Monitor** splits monitors into reactive (failure happened) and proactive (failure foreseeable); the code is written once per subgoal, so no model runs in the loop: +28.7% success, -31.8% time.

In Praxion these checks are exact code over proxy state at 20 Hz. A failed check becomes a typed event, the main path that wakes the LLM.

### 2. Async runtime contract (LeRobot at d3793b7) [C]

A background worker runs the slow policy; a control thread pops actions at a fixed rate.

- **Generation:** a reset bumps it; results from an older generation are dropped silently. `task_version` does the same for task changes.
- **Observation age:** at request start, observations older than 5 s are refused. At acceptance, a source observation past that limit is a **terminal fault** [C].
- **Delay:** turnaround = max of the last 100 request durations. A merge trims min(measured delay, actions consumed), so the splice never runs ahead of the robot.
- **Starvation:** empty queue, then HOLD (position hold), then 1 s grace, then the fault "Active motion buffer exhausted".
- **Provenance:** each action records its request, task version, observation and generation.

Architect: take generation, age checks, HOLD-grace-fault and provenance; skip cursor compare-and-swap and the queue. Praxion's slow tiers emit programs that run for minutes, not 50 Hz actions, so one "current program" slot is enough.

### 3. Commit what runs during the call; plan from its end state (RTC, VLASH) [C]

**RTC** computes the next chunk while the current one executes. The d actions that will run before the result arrives are frozen, and the rest is generated to continue them. Delay = max over a window of recent delays, not the mean.

```
         request              result (d = 4)
            |                    |
old:  ... a0 | a1  a2  a3  a4    | a5  a6     (a5.. replaced)
             +---- committed ----+
new:                             | b5  b6     (continues from a4)
```

On a real robot (pi0.5, 480 episodes), RTC showed no degradation with 100-200 ms of added delay, while synchronous control degraded linearly. Self-reported.

**VLASH** also feeds the model the state *predicted* after the committed actions. In v1, on Kinetix with delay 4, this gave 81.7% against 51.2%.

Transfer (architect): estimate delay as a window max or p95; let the running skill reach a safe-stop point; plan the next program from its expected end state, labelled predicted. Predict only the agent's own deterministic effects (inventory, position after a path), never mobs or drops.

### 4. Parallel reactive tier with stamped guidance; the cost of a running world [C]

**AgileThinker:** the environment advances every N generated tokens. Planner P (R1) streams a plan uninterrupted and restarts on fresh state when done. Reactive R (V3) answers each step with one action, from the current state plus P's partial text labelled "Guidance from a Previous Thinking Model: Turn t1 = {gen_turn}". On the real DeepSeek API, reactive / planning / Agile: Freeway 0.24 / 0.12 / 0.88, Overcooked 0.57 / 0.00 / 0.89. The stale-guidance stamp and the evaluation regime transfer; raw trace sharing does not, as closed hosted models hide reasoning.

**TickingCollab** [V]: Mineflayer skills on Fabric 1.19; GPT-5.1 paused vs real time on three tasks: 0.42 / 0.15, 0.62 / 0.05, 0.28 / 0.06. Architect's caveat: control was roughly one LLM call per action, so it shows per-action control collapses, not skill-level control.

### 5. Event triggers, reaction algebra, adopt-or-cancel [V]

- **FASTER:** with periodic calls, E[reaction] = TTFA + 0.5 × period (TTFA = time to first action). Triggering on events removes the half-period term.
- **SwiftSage** calls the slow model when stuck (5 steps without reward), on an invalid action, a critical decision (fast tier blocked until the slow tier answers) or a surprising observation. Control returns when its ~5-action buffer empties. **Hi Robot** re-plans every 1 s, and at once on a user interjection.
- **Speculative Actions** commits a guessed call only on an exact match: hit chance 1 - (1-p)^k for k guesses, saving capped at 50%, wrong branches must have no side effects. Chess: 54.7% hits, 19.5% time saved.
- **LiveKit** drafts a reply from an interim transcript and keeps it only if the final one matches (`on_preflight_stale` cancels). Its false-interruption handling auto-resumes; Praxion must not copy that, because touching the input hands control to the user at once.

## Landscape

| Work | Kind | Date | What it shows | Availability | Marker |
|---|---|---|---|---|---|
| RTC | paper | 2025-06, NeurIPS 2025 | Frozen committed prefix; no degradation with +100-200 ms delay | MIT reference code; LeRobot port | [C] |
| Training-time RTC | paper | 2025-12 | Delay learned in training; 108 vs 135 ms | LeRobot, kinetix | [V] |
| LeRobot async runtime | project | 2026-10-23 | Generations, age bounds, HOLD/grace/fault | Apache-2.0 | [C] |
| SmolVLA | paper | 2025-06 | Introduced the async inference stack | paper | [V] |
| VLASH | paper | 2025-11, v2 2026-07 | Conditions on the predicted state | Apache-2.0 | [C] |
| FASTER | paper | 2026-03 | Reaction-time algebra | paper | [V] |
| Real-Time Reasoning Gym + AgileThinker | paper | 2025-11, ICLR 2026 | Parallel tiers keep 0.58; a pure planner falls to 0.05 | MIT | [C] |
| TickingCollabBench | benchmark | 2026-06 | Minecraft: 0.65 paused vs 0.15 real time (maxima) | paper | [V] |
| Parallelized Planning-Acting for Efficient LLM-based Multi-Agent Systems in Minecraft | paper | 2025-03, AAMAS 2026 | Single-slot buffer; the LLM sets the interrupt flag | paper | [V] |
| DPT-Agent | paper | 2025-02, ACL 2025 | The LLM rewrites FSM rules | MIT | [V] |
| HLA | paper | 2023-12 | Slow, fast and executor tiers; macro latency 1.07 vs 4.16 s | paper | [V] |
| SwiftSage | paper | 2023-05 | Explicit triggers; 84.7 vs 33.8 for ReAct | code, no license | [V] |
| Lyfe Agents | paper | 2023-10 | Code ends options; $0.51 per agent-hour | paper | [V] |
| Talker-Reasoner | paper | 2024-10 | Answers from a possibly stale belief; qualitative | paper | [V] |
| DoReMi | paper | 2023-07, IROS 2024 | Plan-carried constraints | paper | [V] |
| Code-as-Monitor | paper | 2024-12, CVPR 2025 | Reactive and proactive monitors | paper | [V] |
| TR programs, triangle tables, Gat, event-triggered control | technique | 1972-2007 | Formal basis | papers | [V] |
| ROCKET-1 | paper | 2024-10 | Slow VLM points at targets, a fast tracker follows them; 0.82 | code, no license | [V] |
| OpenHA | paper | 2025-09 | Combat suffers when the fast tier is cut off from reasoning | MIT | [C] |
| Lumine | paper | 2025-11 | First action at ~114 ms; a thought every ~3.2 s | weights not stated | [V] |
| Game-TARS | paper | 2025-10 | Minecraft: sparse 63%, every step 45%, never 55% | closed | [V] |
| LLMCompiler | paper | 2023-12, ICML 2024 | Streamed dispatch gains 1.30x only with slow tools | MIT | [V] |
| Speculative Actions | paper | 2025-10 | Commit on match; 54.7% hits | paper | [V] |
| Speculative Interaction Agents + AsyncLM | paper | 2026-05; 2024-12 | Event-loop agent; SI-SFT | CC BY 4.0 | [C] |
| Are Large Reasoning Models Interruptible? | paper | 2025-10, ICML 2026 | Late updates cost up to 60% | Apache-2.0 | [V] |
| AgenticCache | paper | 2026-04, MLSys 2026 | Plan-transition cache; -65% latency | MIT | [V] |
| LiveKit Agents | project | 2026-10-25 | Preemptive replies, fillers, interruption holds | Apache-2.0 | [V] |
| Figure Helix / Helix 02 | product | 2025-02, 2026-01 | Shared-latent tiers (vendor figures) | closed | [V] |
| DuoCore-FS; AsyncDriver | papers | 2025-12; 2024-06 | Train the fast tier against the slow tier's real delay | AsyncDriver MIT | [V] |
| Hi Robot | paper | 2025-02, ICML 2025 | Re-plans within 1 s or on interjection | paper | [V] |
| Gemini Robotics 1.5 | paper | 2025-10 | Planning and success detection dominate failures (vendor) | closed | [V] |
| GR00T N1 to N1.7 | project | 2025-03 to 2026-10 | One synchronous call per chunk; 22.9 ms on an H100 | Apache-2.0 | [V] |
| HiRT; RoboDual; Fast-in-Slow | papers | 2024-10; 2024-10; 2025-06 | Latent or shared-weight fast/slow models | not checked | [U] |
| RLinf RTC integration | project | unknown | RTC on pi0.5 | URL returned 404 | [U] |

## What Praxion can take

Ranked by the architect's verdict, strongest first.

1. **TR guarded rule programs as the LLM-to-executor interface** (TR, DPT-Agent; strong; sessions 5, 7, 8, 10). A DSL over the fair-play world view only, not eval'd code. Session 5 must draw the line between the plan DSL and skill code.
2. **Plan-carried monitors from a tested template library** (DoReMi, Code-as-Monitor; strong; sessions 5, 7, 8, 11). The LLM fills templates, not free predicates; admission tests catch both false-alarm storms and blind spots.
3. **LeRobot's policy parts** (useful; sessions 4, 7, 8, 14): generation bumped on take-over, goal change or reset; age checks; HOLD, grace, fault; provenance. Reimplement, do not import.
4. **The runway rule** (GR00T, TickingCollab; useful; sessions 8, 14): add a concurrent tier only when a call outlasts its output's runway. Jev calls at skill boundaries can be prefetched, so Jev may need no async machinery.
5. **Plan from the committed skill's end state** (RTC, VLASH; useful; sessions 5, 8, 14). Needs per-skill effect declarations, an open gap in systems-review §4.
6. **Critical-decision class** (SwiftSage; useful; sessions 7, 10): a portal near the base, the last eye of ender, entering the End need slow-tier confirmation.
7. **No mid-reasoning updates** (Interruptible LRMs; useful; sessions 8, 14): validate, or cancel and restart; rate-limit restarts.
8. **Answer chat at once, block only execution** (Talker-Reasoner, HLA, Hi Robot; useful; session 9): only low-stakes first steps before the plan; a task stack makes "carry on" work.
9. **Targets by stable ID plus interaction type** (ROCKET-1; useful; sessions 3, 5, 8), only IDs that pass the session 3 visibility filter.
10. **Combat stays a local reflex or skill** (OpenHA; useful; sessions 6, 7).
11. **Also useful:** evaluate with time passing while models think (session 14); read LiveKit's source for preemption and fillers (sessions 9, 13); log Jev-LLM agreement per context (Speculative Actions; session 8); parse plans incrementally but measure the gain per model (LLMCompiler; sessions 5, 8).

**Weak or already in docs:** FASTER (a one-line formula); training-time RTC (Praxion trains no models); Helix (only: skills should expose remaining time; Jev is latency-bound, not a 3-7 Hz tier, at 80 req/s); Speculative Interaction Agents (generic orchestration); AgenticCache (promote transitions to explicit rules instead); Lumine, Game-TARS, Gemini Robotics, Lyfe (already covered).

## Ideas for Praxion

Strong first; the architect's sharper versions are used.

### 1. Plans as TR programs with template monitors (strong; sessions 5, 7, 8, 10, 11)
- **Mechanism:** rules `{when, do: registered_skill(args), expect, abort_if}`; monitors only as library templates (`lava_near_path`, `durability_vs_work`, `night_eta_vs_shelter_eta`). Admission rejects unknown skills, type errors, more than 30 rules, and rules whose effect satisfies neither a higher guard nor the goal. "No rule holds", an invariant failure or a monitor firing wakes the LLM.
- **Test:** scripted disruptions (pickaxe breaks, creeper, take-over, chest emptied, early night). Pass: at least 80% recovered with no LLM call, vs a linear script from the same prompt.
- **Risk:** inconsistent rule order; the regression check needs effect declarations.

### 2. Latency sweeps with paused and slowed baselines (strong; sessions 8, 14, 15)
- **Mechanism:** Jev +0/150/300/600 ms and LLM +0/2/5/10/20 s on 10 seeds. Baselines via the logged harness channel (RCON), never the agent: `/tick freeze` (paused upper bound), `/tick rate 10` and `5` (slowed time). `/tick` exists since Java 1.20.3; freeze does not freeze players [V].
- **Test:** validate the harness first: a per-step synchronous LLM agent must collapse at 10-20 s while the async design loses under 10 points.
- **Risk:** token and wall-clock cost (use short scenarios); 26.3 client behaviour under `/tick` unverified.

### 3. Take-over with warm resume (strong; sessions 2, 7, 8, 9)
- **Mechanism:** on the first non-injected input event, bump the generation and freeze the program. Release only keys Praxion holds that the user is not physically holding (tracked from the low-level hook). Resume only on explicit hand-back; diff the state, re-plan if it changed a lot, else re-check guards.
- **Test:** take-over under 50 ms; zero user-held keys released; at least 80% of small take-overs resume with no LLM call.
- **Risk:** a change the guards miss resumes a wrong plan.

### 4. Safe-stop contract instead of a step buffer (useful; sessions 4, 5, 8, 13, 14)
- **Mechanism:** each skill declares `{safe_stop_points, expected_end_state, remaining_ms}`. The next program is planned from `expected_end_state` and accepted only if the generation is unchanged and its guards hold on actual state. Camera retargets keep velocity continuous.
- **Test:** 0-20 s injected LLM latency; no heading reversal over 120 degrees within 2 s of a swap.
- **Risk:** poor effect predictions cause rejections. Architect: a cursor-based step list would contradict TR programs.

### 5. Event-triggered slow calls (useful; sessions 7, 8, 10, 14, 15)
- **Mechanism:** a closed enum of causes with typed payloads: program exhausted, monitor fired, skill failed, chat, new threat, progress divergence, take-over ended, ruleset refusal; plus a ~60 s heartbeat that first asks "anything changed?". Per-cause token bucket; repeats coalesced.
- **Test:** 10 injected surprises; lower median reaction than 10 s polling at equal call count.
- **Risk:** a missing cause leaves Praxion blind until the heartbeat.

### 6. Plan the next subgoal early (useful; sessions 8, 10, 14, 15)
- **Mechanism:** launch the call when `remaining_ms` < p95(LLM) + margin; adopt only if the generation is unchanged and guards hold on actual state.
- **Test:** dead time at subgoal boundaries (target p95 < 1 s), adoption rate, discarded tokens.
- **Risk:** wasted calls in chaotic phases (night, Nether).

### 7. HOLD with camera-only fillers (useful; sessions 7, 8, 13, 14)
- **Mechanism:** starvation (goal active, no admitted program, no declared wait) is logged as a fault. Fillers are camera-only or no-op, never inventory changes; after 3-5 s grace a safe fallback goal runs.
- **Test:** proxy shows zero filler-caused world changes; filler clips score no worse than human idle in the blind test.
- **Risk:** uncounted fillers hide starvation.

### 8. Jev's interim pick, promoted to explicit rules (useful; sessions 8, 10, 12, 15)
- **Mechanism:** after the grace deadline (one mechanism with idea 7), Jev picks among code-listed low-stakes options; the LLM directive overwrites it. When agreement in a context passes a Wilson lower bound of 0.9 over at least 20 cases, propose an explicit rule to the user. No hidden cache.
- **Test:** agreement per context, LLM calls saved, zero ruleset violations.
- **Risk:** Jev's first-option bias; "reversible" barely exists in Minecraft.

### 9. Stamped directives (useful; sessions 3, 8, 12, 14)
- **Mechanism:** `{directive_id, generation, task_version, source_tick, model, latency_ms, predicted}` on every directive; Jev's keep-or-abandon questions get the plan's age and at most 5 code-computed diff fields.
- **Test:** staleness-at-execution histogram; A/B of Jev decisions with and without the fields.
- **Risk:** long diffs worsen Jev's weakness with long state.

### 10. Opening move (weak; sessions 5, 8)
One low-stakes skill call emitted before the program, run on parse. Adopt only if it cuts median dead time by at least 1 s; "first step first" would break TR order.

## Challenges to recorded decisions

1. **Smoothness as "longest gap without input"** (session 1.6). Holds.
   - *Problem.* The measure rewards constant input, which hurts naturalness: humans stand still. It is also unclear whether a held key counts as a gap.
   - *Fix.* Measure starvation gaps instead, cross-checked against the proxy: no packets while a goal is active and no wait is declared. Keep the raw input gap as a diagnostic.
   - The user must approve this change.
2. **"The decision layer uses Jev"** needs a scope rule. Holds.
   - *Rule.* No choice with a deadline under ~1 s may depend on Jev or on any hosted call. Jev's measured latency is ~130-320 ms, and Cloudflare measured 524 ms.
   - *Threshold.* Jev decides only when the deadline exceeds its p99 plus a margin, measured from the user's PC.
   - The split of work is still open, so this does not conflict with the decision.
3. **An LLM choosing one skill at a time** (no session-8 design exists yet, so this constrains options). Partially holds.
   - *Evidence.* TickingCollab and AgileThinker show that control with short runway collapses.
   - *Limit.* One skill at a time works when the skill's runway exceeds the next call's latency and that call starts early enough.
   - *Rule.* What binds is runway, not the number of steps.
4. **"Models are hosted"** works only if no hosted call sits on a sub-second path. Holds.
   - Cancelling a call still bills its partial tokens, so restarts count toward cost per hour.
   - Closed hosted models expose only summarized reasoning (some hosted open-weight models stream raw traces); a structured streamed plan schema is the right design either way.

## Open questions

- What are the p50, p95 and p99 latencies of Jev and the candidate LLMs, measured from the user's PC? Does cancelling a call stop billing?
- Which Minecraft decisions have deadlines under a second, and how often do they occur?
- How does the 26.3 client behave under `/tick freeze` and `/tick rate`?
- How accurate are the skill effect declarations?
- Does a TR DSL stay readable at 20-30 rules?
- Do the candidate APIs stream structured JSON while the model is reasoning?
- How often does the proxy see the player's pose? At most once per tick is expected [U].
- How should a prompt mark predicted state as distinct from observed state (session 3)?

## Sources

- https://arxiv.org/abs/2506.07339 · https://github.com/Physical-Intelligence/real-time-chunking-kinetix
- https://arxiv.org/abs/2512.05964
- https://github.com/huggingface/lerobot
- https://arxiv.org/abs/2506.01844
- https://arxiv.org/abs/2512.01031 · https://github.com/mit-han-lab/vlash
- https://arxiv.org/abs/2603.19199 · https://innovator-zero.github.io/FASTER
- https://arxiv.org/abs/2511.04898 · https://github.com/SALT-NLP/RealtimeGym
- https://arxiv.org/abs/2606.15684
- https://arxiv.org/abs/2503.03505
- https://arxiv.org/abs/2502.11882 · https://github.com/sjtu-marl/DPT-Agent
- https://arxiv.org/abs/2312.15224
- https://arxiv.org/abs/2305.17390
- https://arxiv.org/abs/2310.02172
- https://arxiv.org/abs/2410.08328
- https://arxiv.org/abs/2307.00329 · https://sites.google.com/view/doremi-paper
- https://arxiv.org/abs/2412.04455 · https://zhoues.github.io/Code-as-Monitor
- https://arxiv.org/abs/cs/9401101 (Nilsson 1994); Fikes, Hart, Nilsson 1972 (AIJ 3:251-288); Gat 1998; Tabuada 2007 (IEEE TAC 52(9):1680-1685)
- https://arxiv.org/abs/2410.17856
- https://arxiv.org/abs/2509.13347 · https://github.com/CraftJarvis/OpenHA
- https://arxiv.org/abs/2511.08892
- https://arxiv.org/abs/2510.23691
- https://arxiv.org/abs/2312.04511
- https://arxiv.org/abs/2510.04371
- https://arxiv.org/abs/2605.13360 · https://arxiv.org/abs/2412.07017
- https://arxiv.org/abs/2510.11713 · https://github.com/dynamic-lm/interrupt-lrm
- https://arxiv.org/abs/2604.24039 · https://github.com/hojoonleokim/MLSys26_AgenticCache
- https://github.com/livekit/agents
- https://www.figure.ai/news/helix · https://figure.ai/news/helix-02
- https://arxiv.org/abs/2512.20188 · https://arxiv.org/abs/2406.14556
- https://arxiv.org/abs/2502.19417
- https://arxiv.org/abs/2510.03342
- https://github.com/NVIDIA/Isaac-GR00T · https://arxiv.org/abs/2503.14734
- https://arxiv.org/abs/2410.05273 · https://arxiv.org/abs/2410.08001 · https://arxiv.org/abs/2506.01953
- https://rlinf.readthedocs.io/en/latest/rst_source/examples/embodied/rtc.html (404 on 2026-10-25)
- https://minecraft.wiki/w/Commands/tick
