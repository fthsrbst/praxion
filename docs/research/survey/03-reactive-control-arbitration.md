# Reactive control, arbitration and task models

Research date: 2026-10-25. Markers: [V] verified by an independent check, [C] corrected after checking, [D] documented or vendor claim, [U] unverifiable.

Scope: the fast layer. How skills execute, how behaviours compete for the body, how tasks are preempted and resumed, and how an LLM can write reactive structures that survive interruption. Session numbers refer to `docs/discussion-plan.md`.

## In brief

- **Two families make resumption correct.** Condition-driven models (teleo-reactive programs, backchained BTs, BT.CPP reactive nodes) keep no program counter, so resuming is the same as starting. Memory-driven models (stored indices, done flags, SCXML history) are correct only if nothing undid the finished steps. The proxy observes almost every effect, so condition-driven is the default.
- **The condition taxonomy has converged:** entry precondition (checked once), hold condition (checked every tick), postcondition (checked after), plus commitment rules (TeleoR `until`, altoclef `shouldForce`, Baritone `isSafeToCancel`). On resume, check hold conditions, not entry preconditions.
- **Arbitration has three levels:** rank tiers, scored choice within a tier, and resource leases. No Minecraft agent uses leases, and Minecraft's input channels are coupled (WASD is yaw-relative; a GUI captures all input).
- **Bounded commitment beats both extremes.** Replanning on specific triggers beats blind commitment and beats reacting to every event (Kinny & Georgeff). Eager retraction made Soar's reaction 3.5x slower.
- **LLM-written reactive structures work when grounded and gated.** A contract, a static gate and templates raised a weaker model from 50% to 83% task success. Without authoring rules, Claude Code wrote imperative `while` loops.
- **Never resume from LLM self-reports.** After a mid-task addition, the best model finished about 50% of tasks within 10 actions (InterruptBench). Recompute remaining work from proxy-verified state.

Dropped after checking: nothing was refuted. Corrected: Nilsson's limitation is in Section 7, and he contrasts durative with "discrete" (not "ballistic") actions; the TeleoR CEUR paper is "Concurrent Task Programming of Robotic Agents in TeleoR" (RuleML+RR 2017 companion), separate from ICRA 2015; BT.CPP's newest tag is 4.10.0 and its multiple-RUNNING check is off by default; ChatHTN (Apache-2.0) and BETR-XP-LLM (BSD-3-Clause) have code; Parallelized Planning-Acting does not say interrupted actions are never resumed, and its setup uses helpers (teleports, spawned bosses, initial inventory); ProgPrompt's table covers 10 tasks × 5 runs, and the LLM judges assertions from extracted state.

## Key mechanisms

### 1. Teleo-reactive programs with TeleoR commitment and action diffing [C]

A **TR program** (Nilsson 1994) is an ordered list `K1 -> a1, ..., Km -> am`, where `K1` is the goal. On every percept update the first true `Ki` selects `ai`; a durative action runs while its rule stays first. Actions can be TR programs, and higher conditions keep being evaluated, so a change high up preempts everything below. **Regression property:** running `ai` under `Ki` eventually makes a higher `Kj` true. If the rules also cover every state, the goal is reached barring sensing or execution errors. Setbacks fall to a lower rule, luck jumps higher, and resume equals start.

```
get_stone_pickaxe:
  has(stone_pickaxe)                        -> nil
  cobble>=3 & sticks>=2 & table_in_reach    -> craft(stone_pickaxe)
  cobble>=3 & sticks>=2 & has(table)        -> place(table)
  has(wooden_pickaxe)                       -> mine(stone)
```

**TeleoR** (Clark & Robinson) adds what a real body needs:

- **Action diffing.** On each update the new action tuple is compared with the old one: shared actions continue (`move(4.5)` becomes `move(3)`), dropped ones stop and new ones start. In Minecraft this matters for correctness, because releasing and re-pressing W can trigger a sprint.
- **`Guard until U ~> A`** keeps `A` running even if a higher rule fires, until `U` holds; `while W` lets `W` stand in for the guard. Both are a principled safe-to-cancel.
- **Task-atomic resources.** A critical region is entered only when all its resources are free, and it is released when an ancestor fires another rule. A compile-time check prevents deadlock and a FIFO wait queue prevents starvation.

Caveat: with `toggleSneak` or `toggleSprint` on in `options.txt`, "hold" and "release" differ. The input layer must model toggle versus hold per key.

### 2. Condition kinds and halt semantics [C][V]

BT.CPP 4.x (source at 430ea97) specifies the distinctions a skill runtime needs:

| Construct | Semantics |
|---|---|
| ReactiveSequence / ReactiveFallback | Tick every child from index 0 each tick; halt the others when one is RUNNING |
| Sequence / SequenceWithMemory | Resume at the running child; differ only on whether failure resets the index |
| `_failureIf`, `_successIf`, `_skipIf` | Checked only on activation: entry gate |
| `_while` | Checked every tick; false halts the node (SKIPPED): hold condition |
| `onHalted()`, `_onHalted` | Cleanup when halted mid-run (release keys, close GUI) |
| Input-port subscriptions (#1247) | Synchronous callback on a blackboard write: event-driven reaction |

Issue #755 (2024): a minor version changed which siblings ReactiveSequence halts and broke backchaining, fixed in 4.6 with a unit test. Semantics need their own regression tests.

**BDI suspend/resume** (Thangarajah et al. 2008) [V] works at the intention level. Pause the whole subtree first, then suspend top-down. Each suspended goal gets a reconsideration condition whose default action is resume. On resume, check in-conditions and fall back to alternative plans if they fail; entry preconditions are deliberately skipped. Inside a TR skill resume is re-evaluation, so this matters for the session 9 intention stack.

### 3. Backchaining from contracts (PPA) [V]

Colledanchise, Almeida & Ögren grow a reactive BT from goals and an action library:

```
atomic(C) = Fallback(C, Sequence(pre(a1), a1), Sequence(pre(a2), a2), ...)
tree := Sequence(goals)
loop: tick; expand the first failing condition with atomic(C);
      while it conflicts with an earlier branch, move it left
```

Expansion is lazy, and undone conditions are re-achieved without changing the tree. The method has no optimality, assumes a non-conflicting order exists and does not retry failed actions. It works over booleans. Minecraft "obtain X" chains are counted (a table needs 4 planks, sticks need more), and there the count-aware tech-tree solver wins. PPA fits non-count conditions: tool tier, near a table, light, solid footing.

### 4. Commitment: when to reconsider [V]

| Evidence | Result |
|---|---|
| Kinny & Georgeff 1991 | Reacting to every event loses to blind commitment except in very dynamic worlds. With a target-vanished trigger, bold beats cautious everywhere tested. |
| Wray & Laird 2003 (Soar DHJ) | Retract a subtask when any fact it read changes: CPU −39% and −81%, but missile reaction 0.945 s vs 0.268 s |
| Fluid HTN | Replan on world change; accept only a strictly higher-priority method |
| Sutton, Precup & Singh 1999 | Interrupt when `Q(s,o) < V(s)`; value never drops |

All four lead to the same rule: commit boldly, reconsider on a closed set of cheap triggers, and switch within a tier only past a margin that includes the loss from stopping. Survival reflexes bypass the margin.

### 5. Contract-grounded synthesis [V]

Salfity, Anderson & Pryor (2026): a coding agent reads a contract, then writes a BT that must pass a static gate.

```
contract: skills (typed params, success/running/failure), operators,
          templates (e.g. search_pick_selector), world vocabulary
gate:     parses -> allowed operators -> leaves are skills -> params typed, in vocabulary
          on failure: structured errors, max 5 retries
```

Self-reported results (440 simulated and 56 physical trials): Sonnet 4.6 93/90% without templates, 97/96% with them; Gemma4:31b 50/50% vs 83/82%. Synthesis took 50-70 s cold. Templates help weaker models most, and the latency keeps skill writing off the play loop. Supporting results: **Playful** [V]: without eight guidelines, logic leaked into Python loops (script-to-Python ratio 1.0 vs 4.0). **BETR-XP-LLM** [C]: the LLM adds a missing precondition and the fix persists. **ProgPrompt** [C]: `assert(cond) else recovery` comes naturally to LLMs.

### 6. Owner tokens and a `preempted` result (Cairn) [V]

Cairn is AGPL-3.0, so only its ideas can be used.

- Skills return `ok`, `failed` or `preempted`, and preemption is not counted as failure.
- A preempted call re-runs as the same object with its stored `callState`.
- `PathfinderOwner` is the only path for movement writes. A reflex's `acquire` always succeeds and aborts the skill; a stale token's writes are no-ops.
- Deadlines do not grow on resume. Resume waits for a 1,000 ms debounce, guarded by a 30 s watchdog.

Cairn resumes from stored state, not re-checked conditions.

## Landscape

| Work | Kind | Date | What it shows | Availability | Marker |
|---|---|---|---|---|---|
| Teleo-reactive programs | paper | 1994 | Resume = start; regression | open | [C] |
| TeleoR | paper | 2015, 2017 | `until`, action diffing, task-atomic resources | open paper | [C] |
| PPA backchained BTs | paper | ICRA 2019 | Lazy goal-driven reactive tree | open | [V] |
| BehaviorTree.CPP 4.x | project | 2026-10-24 | Entry vs `_while`; halt hooks; port subscriptions | MIT | [C] |
| BDI suspend/resume | paper | AAMAS 2008 | Resume checks in-conditions | open | [V] |
| Kinny & Georgeff | paper | IJCAI 1991 | Trigger-based reconsideration wins | open | [V] |
| Contract-grounded BT synthesis | paper | 2026-07 | Gate plus templates: 50% to 83% | site, no repo | [V] |
| Fluid HTN; Guerrilla HTN | project; talk | 2026-03; 2024-11 | MTR anti-thrash | MIT; proprietary | [V] |
| ChatHTN; HTN method learning | paper | 2025 | Verifier tasks; LLM only as fallback | Apache-2.0 | [C] |
| BETR-XP-LLM | paper | 2024-09 | LLM adds missing preconditions | BSD-3-Clause | [C] |
| Playful | paper | 2026-10 | LLMs default to imperative loops | no license | [V] |
| AgileThinker | paper | 2025-11 | Planner-only 0.92 to 0.05; dual 0.58 | MIT | [V] |
| Utility AI (IAUS, StateTree) | technique | 2015-2026 | Scored choice within a tier | docs | [V] |
| Unreal aborts, GameplayTasks | doc | UE 4.26-5.x | Event aborts; resource overlap policy | docs | [V] |
| Cairn | project | 2026-09-19 | Owner tokens, `preempted`, fixed deadlines | AGPL-3.0 | [V] |
| Parallelized Planning-Acting | paper | AAMAS 2026 | Minecraft; LLM sets the interrupt flag | paper | [C] |
| Nav2, py_trees memory resume | project | 2025-08 | Stored-index resume, pause lifecycle | Apache-2.0; BSD | [V] |
| Soar DHJ | paper | JAIR 2003 | Eager retraction slows reaction 3.5x | open | [V] |
| Interrupting options | paper | AIJ 1999 | Interruption theorem | preprints | [V] |
| SkiROS2 | project | IROS 2023 | Pre/hold/post skill conditions | LGPL | [V] |
| GOAP in F.E.A.R. | talk | 2006 | Focus selection outside the planner | open | [V] |
| ProgPrompt | paper | ICRA 2023 | Assert/else recovery: 0.34 vs 0.28 | code | [C] |
| RoboSafe | paper | 2025-12 | Executable safety predicates | not released | [V] |
| InterruptBench; Progress Bar; Do Not Restart | paper | 2026 | Stale state; unreliable self-reports | InterruptBench code | [V] |
| ReAcTree | paper | AAMAS 2026 | BT around LLM sub-agents: 61% vs 31% | no license | [V] |
| SCXML history | doc | W3C 2015 | Formal memory-based resume | open | [V] |
| Erlang/OTP supervision | doc | current | Restart intensity; staged shutdown | Apache-2.0 | [V] |
| Subsumption | paper | 1986 | Fixed-priority layers | IEEE | [V] |
| Colledanchise & Natale; Iovino et al. | paper | 2021; 2022 | BT execution semantics; BT survey | arXiv | [U] |
| Isla, Halo 2 AI; BTGenBot | talk; paper | 2005; 2024 | Game-BT origin; small-LLM BT generation | open | [U] |

## What Praxion can take

Ranked by the architect's verdict, strongest first.

1. **TeleoR `until` rules and action diffing** (strong; sessions 2, 4, 5, 7). `until` makes safe-to-cancel explicit: sneak stays held until the next bridge block lands. Diffing keeps W held across rule changes, which prevents accidental sprints. Model toggle-mode keys.
2. **Semantics regression suite and synchronous halt with cleanup** (BT.CPP; useful; sessions 5, 14). Copy the distinctions, not the C++ library; the language is open until session 15.
3. **`preempted` as a result, excluded from Wilson statistics; deadlines that never grow on resume** (Cairn; useful; sessions 5, 7, 14).
4. **Event-driven aborts** (Unreal; useful; sessions 2, 7). The protocol already sends deltas, so reflex triggers need no polling.
5. **In-conditions on resume and a reconsideration condition per suspended intention** (BDI; useful; session 9), for the intention stack, not inside skills.
6. **PPA for non-count "ensure" conditions** (useful; sessions 5, 11). Leave quantities to the solver.
7. **Reactive composition templates** (contract-grounded synthesis; useful; session 11). The 50% to 83% gain motivates a measurement; it is not a planning figure. Generate the contract from the runtime registry; no MCP server is needed.
8. **Precondition repair as a small diff** (BETR-XP-LLM; useful; session 11), only for failures typed `precondition` or `world-changed`.
9. **Authoring guidelines plus a lint, with `ensure(cond) else achieve(...)` as the DSL syntax** (Playful, ProgPrompt; useful; sessions 5, 11).
10. **Additions, revisions and retractions in the command suite** (InterruptBench; useful; session 14): "also bring coal", "make it 10", "never mind".
11. **Restart intensity per (skill, target, window)** (Erlang; useful; sessions 5, 7). Unify it with altoclef's blacklist. Forced release must be ordered (camera and attack first, sneak last) and never runs on take-over.

**Weak or already in docs:** TR semantics (survey/02 rank 1; extended by idea 1); Kinny & Georgeff, AgileThinker, Parallelized Planning-Acting, memory-based resume, SkiROS2, ChatHTN verifiers, subsumption (recorded); utility curves (3-6 options per tier do not justify tuning); Fluid HTN (only MTR, a form of hysteresis); DHJ, interruption theorem, GOAP, RoboSafe, ReAcTree, SCXML.

## Ideas for Praxion

Strong first; the architect's sharper versions are used.

### 1. One TR DSL for plans and skills, with compiler-derived guards (strong; sessions 4, 5, 7, 11)
- **Mechanism:** the LLM writes ordered steps `{action(args), goal}`. The compiler derives guards by count-aware regression over primitive effects generated from the version pack, so the guard before crafting a table requires planks ≥ 4 plus what later steps need. It rejects steps that raise no guard. The LLM's `ensures` is a cross-check only. This is the same DSL as the plan programs in survey/02 idea 1.
- **Test:** 20 obtain/ensure skills in the fake world, with undo-perturbations at step boundaries. Pass: 0 duplicate placements, 0 wasted crafts, and derived guards match hand-written ones in 100% of cases.
- **Risk:** effect declarations must be exact; combat micro needs free leaf code.
- **Architect note:** LLMs will write wrong regression guards for consumables, so the compiler must derive them.

### 2. Interruption-injection admission test (strong; sessions 5, 7, 11, 14)
- **Mechanism:** inject at every primitive boundary and at the start, middle and end of each durative primitive. There are four kinds: reflex, take-over then hand-back, revised command, undone postcondition. Proxy-log checks: the postcondition is reached within 1.5x the uninterrupted time; no duplicate irreversible effect; items are conserved modulo declared consumption and despawn; no held keys or open GUI one tick after preemption.
- **Test:** seed resumption bugs (a done flag, a cached target) into hand-written skills. The runner must catch them all before it gates admission.
- **Risk:** live runs are slow, so most coverage depends on the fake world's fidelity.

### 3. Cancellation contract per primitive (strong; sessions 2, 4, 5, 7)
- **Mechanism:** `{cancel: free | lossy(expected_loss) | unsafe_until(cond, max_ticks) | compensable(action)}`, merged with survey/02's safe-stop contract. Examples: walking is free; digging is lossy; a mid-gap jump or a bridge-edge sneak is unsafe_until; an open chest is compensable. On take-over, injection stops at once. Unsafe_until keys stay held until the user touches that key or any movement key, or `cond` holds; all other keys are released.
- **Test:** take over at every phase of bridging, pillaring, eating, chest use and bucket use. Pass: zero hand-over fall damage, zero stuck keys, under 50 ms.
- **Risk:** wrong contracts at edge cases; compensation can itself fail.
- **Architect note:** without this, releasing a held sneak mid-bridge walks the player off the edge.

### 4. Two-level leases over the body (useful; sessions 2, 4, 7, 13)
- **Mechanism:** an exclusive mode lease (`WORLD | GUI | CHAT`) with three leases inside WORLD: camera, locomotion and hands. Locomotion is yaw-dependent and recomputes its keys from the current yaw every tick. Epoch-stamped tokens; stale events are dropped.
- **Test:** path-tracking error with and without a separate camera owner; zero injected events after revocation; zero accidental sprints.
- **Risk:** 8 key directions may make split ownership track paths worse.

### 5. Tiered arbiter with a loss-aware margin (useful; sessions 7, 8, 9, 13)
- **Mechanism:** take-over bypasses the arbiter entirely. The tiers inside are survival, then the user command, then maintenance, then own projects. Each need has a soft threshold that runs at the command's next safe point and a hard threshold that promotes it to survival (altoclef ranks food above the user task). A same-tier switch needs a gain greater than k × `loss_on_interrupt` and no active `until`.
- **Test:** pass if A→B→A oscillations within 5 s = 0, tier-1 preemption ≤ 1 tick, and no starvation deaths in 30-minute commands.
- **Risk:** k and the loss estimates must be found empirically.

### 6. Reconsideration filter: hard triggers first (useful; sessions 7, 8, 10)
- **Mechanism:** ship hard triggers plus a heartbeat and log every ignored event. Add a Jev yes/no question per event type only when replayed labels show the rules miss more than X% of worthwhile opportunities or wake the LLM more than Y times per hour. Calibrate on at least 200 labels.
- **Risk:** replay validates classification, not outcomes (survey/01's off-policy warning).

### 7. Contract file and gate for LLM-written skills (useful; sessions 5, 8, 11)
- **Mechanism:** a single file generated from the registry, listing primitives, cancellation classes, templates (search-then-act, ensure-tool-then-gather, approach-until-in-reach, retreat-and-return) and the version-pack vocabulary. The gate rejects unknown ids, unobservable conditions, loops and timers, and unbounded oscillating rules.
- **Test:** 50 skill tasks, with and without templates, on a frontier model and a cheaper one. Adopt templates if the cheaper model gains ≥ 20 points.

### 8. Smaller items
- **Receipt epoch** (already in docs; sessions 3, 5, 9): a receipt counts for intention I only if its tick is after I's start epoch.
- **Read-set invalidation** (weak): poll all guards each tick and adopt read sets only if p99 evaluation exceeds 2 ms. Keep read sets as a log field.
- **HTN above TR** (weak): defer to session 10 until logs show thrashing or LLM calls per rung that do not fall.

## Challenges to recorded decisions

1. **Session 2.7 needs a delta event stream and a decode-to-arbiter latency budget.** Holds; it is the only challenge here that is new. Reflexes sit downstream of the separate decoding process. The protocol is already delta-based, so the real cost is the extra hop. The client also reports its position at most once per tick, adding up to 50 ms of perception lag. Proposal: p99 under 5 ms on the user's PC. This does not reject the separate process.
2. **Narrow "LLMs may write code, for skills" (2026-10-21) in session 11.** Holds as a proposal.
   - *For:* imperative code does not resume correctly and LLMs default to it (Playful), while templates and a gate help weaker models.
   - *Against:* a DSL is still code, and survey/01 and survey/02 already argue this. The evidence comes from robots. Free leaf code must remain possible (combat, bucket clutch).
3. **Cairn's 1 s debounce versus "input gap <1 s".** Holds partly. Under survey/02's starvation-gap definition, a short declared settle is legal. The rule is: no undeclared fixed debounce. The blind test judges naturalness.
4. **Keep Jev off the reflex path.** Holds, but this is already recorded (notes.md, survey/02).

## Open questions

- Does 26.3 reset dig progress when attack is released or the held item changes? Very likely [U]; this sets digging's `loss_on_interrupt`.
- Can every needed postcondition be computed within the session 3 fair-play perception policy?
- After a hand-back, should suspended intentions resume automatically or wait for a command, given the user may have changed the world?
- How does the body tell physically held keys from injected ones on Windows?
- How accurate are version-pack effect declarations for random drops and tool-dependent yields?
- Should skills be stored as TR rule lists, BT XML or Praxion's own format? This affects session 15.
- Do the unread missed works change any conclusion?

## Sources

- https://arxiv.org/abs/cs/9401101
- https://ceur-ws.org/Vol-1875/intro1.pdf
- https://arxiv.org/abs/1611.00230 · https://arxiv.org/abs/1709.00084
- https://github.com/BehaviorTree/BehaviorTree.CPP
- https://ifaamas.csc.liv.ac.uk/Proceedings/aamas08/proceedings/pdf/paper/AAMAS08_0056.pdf
- https://www.ijcai.org/Proceedings/91-1/Papers/014.pdf
- https://arxiv.org/abs/2607.12220 · https://jsalfity.github.io/agentic-bt-gen-webviewer
- https://github.com/ptrefall/fluid-hierarchical-task-network
- https://arxiv.org/abs/2505.11814 · https://arxiv.org/abs/2511.12901 · https://github.com/hhhhmmmmm02/ChatHTN
- https://arxiv.org/abs/2409.13356 · https://github.com/jstyrud/BETR-XP-LLM
- https://arxiv.org/abs/2610.17083
- https://arxiv.org/abs/2511.04898 · https://github.com/SALT-NLP/RealtimeGym
- https://dev.epicgames.com/documentation/unreal-engine/statetree-selectors-overview
- https://dev.epicgames.com/documentation/en-us/unreal-engine/API/Runtime/GameplayTasks/ETaskResourceOverlapPolicy
- https://github.com/VasilisDragon/cairn
- https://arxiv.org/abs/2503.03505
- https://github.com/ros-navigation/navigation2
- https://arxiv.org/abs/1106.4871
- https://doi.org/10.1016/S0004-3702(99)00052-1
- https://github.com/RVMI/skiros2 · https://arxiv.org/abs/2505.04493
- https://www.gamedevs.org/uploads/three-states-plan-ai-of-fear.pdf
- https://arxiv.org/abs/2209.11302 · https://progprompt.github.io
- https://arxiv.org/abs/2512.21220
- https://arxiv.org/abs/2604.00892 · https://github.com/HenryPengZou/InterruptBench
- https://arxiv.org/abs/2609.08589 · https://arxiv.org/abs/2609.13800
- https://arxiv.org/abs/2511.02424
- https://www.w3.org/TR/scxml/
- https://www.erlang.org/doc/system/sup_princ.html
- https://doi.org/10.1109/JRA.1986.1087032
- https://arxiv.org/abs/2106.15227 · https://arxiv.org/abs/2005.05842 · https://www.gamedeveloper.com/programming/gdc-2005-proceeding-handling-complexity-in-the-halo-2-ai · https://arxiv.org/abs/2403.12761
