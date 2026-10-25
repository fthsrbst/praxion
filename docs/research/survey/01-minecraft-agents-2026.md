# Minecraft agents and products: 2025-2026 frontier

Research date: 2026-10-25. Markers: [V] verified by an independent check, [C] corrected after checking, [D] documented or vendor claim, [U] unverifiable.

Scope: who has beaten the game, how they did it, and who is closest to Praxion. Session numbers refer to `docs/discussion-plan.md`; 2.x are the sub-topics of session 2.

## In brief

- **LLM agents beat the game three times in Sep-Oct 2026.** All three ran on Mineflayer, as single runs, with privileged perception:
  - a planner + Jev on Peaceful, on a speedrun seed with an active End portal (8:43);
  - Claude Opus 5 with 37 engineered tools on Normal (3 h 56 min, 1 death);
  - four Gemini 4 Argon coding agents on Easy (27 h 14 min, 605 deaths).
- **No agent has finished the game through keyboard and mouse, with line-of-sight perception, or more than once.** No reliability number exists. Praxion's many-seed milestone ladder would be the first.
- **In the one comparison available, the engineered body beat free-form code, but the comparison is confounded.** The runs differ in model, agent count, difficulty, tools and seeds. The 1-vs-605 deaths gap is suggestive, not proof.
- **The fast-decision pattern has converged.** A planner runs at milestones, code lists the valid options, and Jev picks one in about 0.2 s. Calling Jev on every state change fails: 20 calls/min, all for one unchanged situation. The harm is flip-flopping, stacked latency and rate limits; the cost is only about $0.35/h.
- **Verification has converged:**
  - receipts carry observed evidence;
  - truth is three-valued, and UNKNOWN never passes;
  - an external referee judges verdicts.
- **Pixel and VLA agents are far from the endgame.** Dreamer 4 gets diamonds in 0.7% of episodes, and GUI work is weakest. These are perception and policy failures, so they say little about Praxion's OS-input risk, which is mechanical.
- **Products put an LLM on top of a hand-written engine.** None learns verified skills. No Microsoft or Mojang agent was found [U].
- **No Minecraft-specific way to evaluate naturalness exists.** Methods from other domains do: BotPrize's protocol with a human baseline, and learned human-versus-agent detectors.

**Dropped after checking.** No item was refuted. These claims were removed because the source does not support them:
- Dreamer 4's "about 21 fps": not stated.
- Gemini's "End fight at about 14 h": not in the paper. Crystals started at about 21 h.
- AHB's "robotic linearity" quote: not in the abstract.
- "Easy helped because hunger cannot kill": hunger cannot kill on Normal either.

## Key mechanisms

### 1. Non-blocking action protocol with evidence receipts (Mine AI MCP) [V]

```
brain                                   body (one foreground lock)
submit{kind,args,submission_id,     ─▶  accepted | pending{progress} | settled{receipt}
       rationale,wait_timeout_ms}
wait_for_action(id)                 ─▶  progress + deltas (tools, combat, reflexes)
submit before reading settled       ─▶  RESULT_NOT_RETRIEVED
second foreground submit            ─▶  ACTION_BUSY
```

- **Receipts.** A receipt reports only evidence the body observed. `settleInventoryCount` waits a few ticks for the expected count, because Mineflayer resolves on the server's first witness.
- **Confirmed flag.** `confirmed: false` means "the last value seen", not "failed".
- **Crystals.** A crystal counts as destroyed only after an observed death or an explosion at its position. Otherwise the result is `shot_missed`.
- **Resumable tasks** keep their inventory baseline across reflex takeovers.
- **Effect.**
  - About 92% of wall time was spent waiting on the world; only 17.8 min went to API calls.
  - The model can never skip reading an outcome.

### 2. Reflexes under a typed survival policy (Mine AI MCP) [V]

- **Six reflexes** share one driver: fire, breath, footing, dragon, hostile and hunger. Each runs observe → pure decision → execute → settle.
- **The policy** is the model's only lever on autonomous behaviour. It is a set of typed fields, for example `engage_min_health`, `critical_health`, `evade_safe_range` and `bucket_fall_save`.
- **Edits** carry an `expected_revision`, a `reason` and a lifetime (session, encounter, until a condition holds, or up to 1 h). Death or a dimension change resets the policy.
- **Safety stays in code.** `destroy_end_crystal` computes the explosion damage and refuses with `CRYSTAL_BLAST_UNSAFE` when the result, with a 2-point margin, would breach critical health.
- **Testing:**
  - a 1-minute unit suite;
  - private "Mine Labs" scenarios, judged by a referee from world facts;
  - live playtests.

  Every incident becomes a regression test.
- **Library forks.** Its forks of mineflayer, prismarine-physics, prismarine-recipe and minecraft-data fix placement acks, inventory sync, partial pickups, float drift and recipe remainders. These are game-logic edge cases that Praxion's world model must also handle.

### 3. Milestone planner, enumerated options and Jev selection (rmalde; teknium1) [C]

```
planner LLM   only at: start | targets met | waypoint reached | 2 failures | every 8 actions
              → {objective, targets{item:min}, waypoint{x,z}}
GET /options  → [a0..an] executable AND useful now
Jev choice    → probabilities + confidence     (median 227 ms)
POST /act ak  → one bounded action; every decision logged
```

- **Correction.** The endpoints, the replan interval and the plan JSON come from teknium1's MIT reimplementation, not from rmalde's code.
- **Doctrine.** Bad behaviour is fixed by changing the options offered, never the prompt. For example, mining stops being offered once the targets are met.
- **Bed attack.** The bot aims first, then fires when the observed dragon head enters the attack window. This raised damage from 11 to 46 per bed. The head position comes from a server-side hook, so it is privileged information.
- **Limits.**
  - The seed has an active End portal, so the run needed no blaze rods, eyes of ender or stronghold.
  - On a known route the planner is nearly a script, so 131 decisions per run will not carry over to other settings.

### 4. Decision epochs and a typed decision plane (souta-lab) [V]

Pipeline: verified world model → grounded transitions → Jev → kernel leases → actuation → VERIFY ∈ {TRUE, FALSE, UNKNOWN}. UNKNOWN never passes.

Its 975-line best-practices document sets these rules:
- **State.** One canonical `DecisionState` with one representation per fact. Code precomputes exact facts.
- **Candidates** are semantic commitments with stable IDs. The independent decision heads are batched over one snapshot.
- **Epoch causes.** Every call carries a typed `decisionReason` from a closed set:
  - candidate set changed;
  - lease ended;
  - health, food or air band crossed;
  - hostile in range;
  - no progress.

  Heartbeats and small moves are forbidden as reasons.
- **Commitment.** An admitted action is held until VERIFY, a failure or a real interrupt.
- **Missed deadline.** Keep the current action or hold safely. Never invent a fallback.
- **Freshness.** Answers are bound to the state hash, and stale answers are dropped.
- **Low confidence.** Escalate or hold. Never silently take the runner-up.

**Measured failure without gating:** 1,215 calls in one hour at about 7k tokens each, all for one situation (explore a cave, one MOVE candidate). The status is also a warning: seven failing test files, no clean Nether pass [D], and governance built before a working body.

### 5. Coding agents on a raw bot API (Gemini 4 Argon team) [V]

- **Program interface.** Each agent sends JavaScript programs `async (bot) => {...}` to its Mineflayer bot.
  - Values stored on `global` persist between programs. The result was an emergent helper library: 42 helpers used in 1,519 programs.
  - Each program returns its logs, its return value and any error.
- **Loop.** Write an observe program, read the result, write an act program. Each body ran a program only about 40% of the time.
- **Coordination.** Chat messages are injected into each agent's next program output. A shared memory holds chests, portals, dangers and tool quirks.
- **Harness.** Saves every 5 minutes and restarts after failures (6 times). An Observer agent audits claims against the record. One false success claim was retracted after 2 minutes.
- **Emergent techniques:**
  - triangulating the stronghold from two eye-of-ender throws sampled every 50 ms (43 m error);
  - shelters for luring Endermen;
  - dirt pillars to reach the End crystals.

### 6. Camera dynamics (rmalde `camera-control.mjs`) [C]

A per-axis, velocity-limited P-controller. The parameters are confirmed; the structure is inferred from them:

```
v_cmd = clamp(12/s · err, ±240°/s);  v += clamp(v_cmd − v, ±960°/s² · dt);  settled at |err| < 0.0045 rad
```

- **Walking keys** come from projecting yaw onto the path heading (threshold 0.383, cos 67.5°).
- **Look-ahead.** The camera looks 5 blocks ahead.
- **Enderman guard.** If the view ray hits an Enderman's eyes, pitch is forced to −0.75.
- **License.** The code has none: use the ideas only.

## Landscape

### The three 2026 completions

| | rmalde Astra + Jev [C] | Mine AI MCP + Opus 5 [V] | Gemini 4 Argon team [V] |
|---|---|---|---|
| Version, difficulty | 1.16.5, Peaceful | 1.21.4, Normal (`docker/prepare.mjs`) | 1.20.4, Easy |
| World | fixed seed: village, chest obsidian, active End portal | seed 97996358 | random |
| Privileged information | server-side dragon-head hook | all loaded chunks; dragon health and phase | hidden blocks; dragon state |
| Time | 8:43 (repro 7:45) | 3 h 56 min | 27 h 14 min |
| Deaths | 0 | 1 | 605 |
| Calls | 131 Jev + 35 planner | 388 tool calls | ~24k; 2,783 programs, 22% errored |
| Cost | $0.97 (~$6.7/h) | $93.17 API-equivalent (~$23.7/h) | $520 at introductory price (~$19/h); $1,040 at standard price |
| Reproduced | yes (teknium1) | no; code and data public | no; nothing released |

### Everything else

| Work | Kind | Date | What it shows | Availability | Marker |
|---|---|---|---|---|---|
| souta-lab minecraft-jev-agent | project | 2026-10-12 | decision epochs; three-valued VERIFY; measured ungated-Jev failure | MIT | [V] |
| Anahadd, RayB-007 jev-minecraft; ellistev demo | project | 2026-09/10 | Jev per action; local OpenJev; the original TypeSafe demo | no license; descriptions only | [D] |
| altoclef + ChatClef | project | 2021-05-24 (win); 2025-09-02 | a client mod plays the user's own player; the LLM picks one command at a time | MIT / AGPL-3.0 | [V], win [D] |
| PlayerEngine / ai-companion | project | 2026-10-13 | forced JSON (9 of 21 turns unparseable without it); throttling | LGPL-3.0 | [C] |
| Steve (YuvDwi) | project | 2026-07-28 | Forge entities; LLM output becomes actions or GraalVM JS | no license | [V] |
| Mindcraft-CE + Andy-4.2 | project, model | 2026-10-19 | 9B fine-tune on 2,748 examples; MC up to 1.21.11 | MIT; Andy 2.0 License | [C] |
| dsh-mc-client | project | 2026-10-18 | real OS input to a 26.1.2 client; refuses input when focus fails | MIT, v1.2.0 | [C] |
| mineflayer-reflection-proxy | project | 2022-02; fork 2026-03-08 | a real client and a bot share one connection; the client desyncs | none; fork MIT | [C] |
| Dreamer 4 | paper | 2025-09-29 | diamonds in 0.7% of 60-min episodes, from pixels | no code | [C] |
| HiSTEVE 1.0 | paper | 2026-10-22 | VLA at 20 Hz; GUI 27.2%; 25% on long goal-only tasks | repo 404 | [V] |
| OpenHA / Chain of Action | paper | 2025-09-13 | no single action space is best | MIT | [V] |
| Optimus-3 | paper | 2026-02-10 (v2) | mixture-of-experts generalist | MIT | [V] |
| SIMA 2 | product | 2025-11-13 | lists precise keyboard/mouse control as an open problem | research preview | [V] |
| Project Sid / PIANO | paper | 2024-10-31 | one controller conditions both speech and action | report only | [V] |
| PUBG Ally ("AI Duo") | product | 2026-06-17 | 2B SLM plus a behaviour-tree System 1 | proprietary | [C] |
| Microsoft Copilot, MineWorld | product, paper | demo 2024-05; Gaming beta ~2025-05-28; MineWorld 2025-04-11 | advice, not agency | mixed | [C]; no 2026 agent [U] |
| ReplayMod / ReplayStudio | technique | ongoing | `.mcpr` is a timestamped server-to-client packet dump | LGPL-3.0 | [V] |
| PSN v2 | paper | 2026-06-22 | skill graph; no new numbers, no code | paper | [V] |
| BotPrize 2012 | benchmark | 2012-09 | bots judged ~52% human; humans ~40% | historical | [V] |
| NTT; AHB; GuardAC | technique | 2021; 2026 | learned human-versus-agent detectors (AHB: mobile touch); sensitivity-grid check | mixed | [C] |
| VPT; PLAICraft | dataset | 2022; 2026 | human keyboard/mouse data at scale | VPT public; PLAICraft unclear | [V] |

## What Praxion can take

Ranked by the architect's verdict, strongest first.

1. **Mine AI MCP as the reference for receipts, resumable tasks, the reflex policy and the testing doctrine** (strong; sessions 4, 5, 7, 14).
   - Read it; do not import it. It is about 100k lines of TypeScript on a protocol-level body.
   - Praxion's reflexes pay input-to-effect latency that MCP never paid.
2. **souta-lab's decision-epoch discipline** (useful; sessions 8, 14). Read its best-practices document before session 8.
3. **Focus refusal, from dsh-mc-client** (useful; session 2.3). Send no input unless `GetForegroundWindow` belongs to Minecraft and no unexpected screen is open. This is a body invariant.
4. **"Change the offered options, not the prompt"** (useful; session 8). Also take the camera limits and the Enderman guard as a starting point for motor control (session 13; ideas only).
5. **From the Gemini run** (useful; sessions 3, 10, 14): eye triangulation, harness snapshots and restarts, and its failure rates as a negative baseline.
6. **One decision conditions both the reply and the action, as in Project Sid** (useful; session 9).
7. **The blind-test design from BotPrize** (useful; sessions 13, 14):
   - always include human clips, judged by the same panel;
   - collect enough judgments;
   - stratify the judges.
8. **An automated humanness detector as a per-build metric** (useful; sessions 13, 14). It will find trivial tells first, for example walking with no mouse motion.
9. **VPT and PLAICraft as a population sanity check** (useful; session 13). Check their licenses first.

**Weak or already covered.**
- Weak: a raw packet log, not ReplayMod, for incident review (session 14); Optimus-3's QA data for RAG tests, after license checks (session 12).
- Already covered: altoclef, SIMA 2 and PSN.

## Ideas for Praxion

Strongest first. Where the architect gave a sharper version, it replaces the original.

### 1. A personal motor model learned from the user's own play (strong; sessions 4, 13)
- **Idea:** Praxion moves like the user, so take-over and hand-over look like the same person.
- **Mechanism:** Praxion captures the user's input with its own Raw Input sink (`RIDEV_INPUTSINK`, about 1 kHz) and a keyboard hook. The proxy adds the target context: what the crosshair settled on. From this data Praxion fits distributions for:
  - turn velocity against amplitude;
  - overshoot and settle time;
  - reaction delay;
  - key overlap;
  - hotbar habits.

  The motor controller samples movements from these distributions.
- **Test:**
  - KS or energy-distance tests are not significant on held-out sessions;
  - the detector's AUC is at most about 0.6.
- **Risk:** Logging needs consent and local-only storage. Precision tasks may need to override the user's style.
- **Architect note:** This is the best naturalness idea. A low-level mouse hook only sees recentred cursor positions, which is why a raw-input sink is needed.

### 2. A reflex coverage matrix with refereed first-night fixtures (strong; sessions 7, 14)
- **Idea:** Make "flawless first demo" testable, one threat at a time.
- **Mechanism:** Each cell is (threat, distance, light, terrain) → a pass predicate over server facts, plus a latency budget. Threats include creepers, skeletons, lava, drowning, falls and Enderman gaze. The logged harness sets up each fixture; Praxion's own report never decides a verdict. Each cell names the layer that owns it.
- **Test:** The demo gate requires all three:
  - 100% on reflex cells, over 10 instances each;
  - at least 18 of 20 fresh-seed first nights without death;
  - every observed death added as a new cell.
- **Risk:** Overfitting to staged fixtures; the fresh-seed runs compensate.

### 3. Evidence receipts with per-action rules (useful; sessions 4, 5, 14)
- **Idea:** Each action reports how strong its proof is, not just success or failure.
- **Mechanism:** A receipt's status is confirmed, refuted, unrefuted or unconfirmed, and it records whether the input came from Praxion or from the user.

  | Action | Evidence |
  |---|---|
  | Dig | the block is air after the matching `block_changed_ack`; the proxy pairs the client's sequence number with Praxion's own input |
  | Pickup | its own receipt: the item entity is collected |
  | Slot click | hands-confirmed by the client's packet; world-unrefuted if the server sends no correction within N ticks |
  | Look, move | the client's packets agree, and the server sends no correction |

- **Test:** Inject faults. False-confirmed must be 0.
- **Risk:** If settle windows are wrong, receipts become slow or misleading.
- **Architect note:** Since 1.17, `stateId` is never a positive witness.

### 4. A reflex policy the LLM may tune, bounded by the ruleset (useful; sessions 7, 8, 9)
- **Idea:** The LLM adjusts reflexes only through a small typed policy that the user's ruleset caps.
- **Mechanism:**
  - The policy has about 10 typed fields.
  - Each edit carries a revision and a lifetime from a closed set: encounter, dimension, or a duration.
  - Protected blocks and items act as hard bounds.
- **Test:**
  - A fuzzed property test shows that no edit sequence widens the ruleset.
  - First-night fixtures pass with the default policy alone.
- **Risk:** The LLM may thrash the policy.
- **Architect note:** The demo must survive a model outage, so it cannot depend on tuning.

### 5. A decision-epoch gate with hysteresis (useful; sessions 8, 14, 15)
- **Idea:** Jev is asked only when the decision itself changes, and a new answer must clearly beat the current one.
- **Mechanism:** Every request carries an epoch cause from a closed set and a state hash. A new action replaces the committed one only if P(new) − P(current) exceeds a margin. All decisions are logged.
- **Test:**
  - About 0 calls during committed actions, and 0 flip-flops.
  - Replay is used for regression only.
  - Outcomes are compared only in live runs on the same seeds.
- **Risk:** Too few epochs make Praxion slow to react.
- **Architect note:** Replaying recorded outcomes to score a new state compiler is off-policy evaluation, and it misleads.

### 6. A body that never freezes while the brain thinks (useful; sessions 5, 7, 8)
- **Idea:** The body never waits for the brain.
- **Mechanism:**
  - The protocol is MCP-style: submit, pending, settled.
  - Commands, take-over and reflexes preempt without a handshake.
  - A "freeze" counts only when there is no input while an action is committed or a threat is near.
- **Test:** Inject 5, 15 and 30 s of LLM latency:
  - the maximum freeze stays under 1 s;
  - preemption stays under 50 ms.
- **Risk:** An "idle repertoire" that opens the inventory is dangerous.
- **Architect note:** The Gemini 60% idle figure is not a comparable baseline.

### 7. Fair-play stronghold finding and the dragon phase as a policy question (useful; sessions 3, 6, 10)
- **Idea:** Find the stronghold from visible eye flights, and treat the dragon phase as a perception-policy question rather than a classifier problem.
- **Mechanism:**
  - Keep a Bayesian posterior over stronghold chunks with the ring prior, as Ninjabrain Bot does [U]. Throw another eye only if the best posterior is below 0.9.
  - The client probably receives the dragon phase as synced data (`DATA_PHASE`) [U: verify in the 26.3 jar]. Session 3 decides whether metadata that is received but not displayed may be used.
- **Test:** Over 50 seeds, measure the chunk-hit rate.
- **Risk:** A phase error at landing is costly.

### 8. One decision drives both chat and action (useful; session 9)
- **Idea:** What Praxion says always matches what it is doing.
- **Mechanism:**
  - Replies are conditioned on the committed decision.
  - Replies go through the separate interface. In-game chat is used only for short acknowledgements at safe moments, because typing stops movement for about 1-3 s.
  - The proxy checks that the acknowledged action starts.
  - Hook flags separate Praxion's chat from the user's.
- **Test:** The incoherence rate is 0.
- **Risk:** Replies arrive later.

### Weak ideas
- **Skill promotion mined from usage** (sessions 5, 11): premature. If it is built, use an explicit "scratch" import namespace with Wilson-bounded promotion.
- **An Observer model** (sessions 8, 14): instead, every claim must reference a receipt id, and an LLM audits reports offline.
- **ReplayMod for blind tests** (sessions 13, 14): instead, record the real window with OBS and run a paired forced-choice test.
- **Jev scores for gathering priority** (sessions 5, 8, 10): use solver costs instead, and Jev only at strategic epochs.

## Challenges to recorded decisions

1. **Hosted models need an explicit degraded mode** (hosted models; session 1.6). This holds and is the strongest challenge.
   - Evidence:
     - souta-lab's campaign stopped when its provider credits ran out;
     - Mine AI MCP's client hit a 5-minute HTTP idle timeout [D].
   - Requirements:
     - take-over, reflexes and ruleset enforcement must not depend on the network;
     - every call class gets a deadline and a defined fallback.
   - Test: a chaos test that cuts the providers mid-night. Sessions 7, 8, 15.
2. **Narrow "LLMs may write code"** (session 1). This holds, though the comparison behind it is confounded. Survival-critical code should be human-reviewed and tested, and LLM skills should compose only on that verified layer. Propose this as a trust-tier rule in session 5.
3. **"Hands on the keyboard" has no precedent at endgame difficulty** (session 1.5). This holds as a call to test.
   - The unknowns are mechanical: latency, jitter, tick timing and pointer acceleration.
   - Session 2.7 should test primitives:
     - input-to-packet latency;
     - a one-tick key hold;
     - aiming within 1° in 150 ms;
     - a tick-timed click;
     - one integrated test, the water-bucket save.
4. **Zero cheats makes the target harder than every published run** (session 1.3). This holds partially.
   - The proxy receives what Mineflayer receives, so session 3's visibility policy decides how much harder.
   - Next step: run an ablation with perception profiles on the same seeds.

**Not upheld:**
- *The MCP forks as evidence for MCProtocolLib.* The forks fix game logic, not decoding.
- *"Jev at epochs, not on state changes".* This is already recorded in decision-models.md.

## Open questions

- **Reliability:** would any whole-game agent succeed again on other seeds?
- **Timing:** can SendInput meet endgame timing with per-frame mouse sampling?
- **Fair perception:** what does each withheld fact cost?
- **Hidden metadata:** what is the policy for metadata that is received but not displayed (session 3)?
- **Jev at scale:** does Jev stay calibrated with 20-50 options per choice?
- **Humanness:** which kinematic features separate human from bot play in Minecraft?
- **Gemini harness:** will its internals be published?
- **Referee:** is there an open equivalent of Mine Labs?
- **Others' status:** what is the status of SIMA 2, Player2 and Microsoft?
- **Data:** what are the licenses for the VPT and PLAICraft data?

## Sources

- Gemini 4 Argon: https://arxiv.org/abs/2610.22319 ; press: https://thenextweb.com/news/gemini-4-argon-agents-beat-minecraft-27-hours
- Mine AI MCP: https://github.com/aibengineering/mine-ai-mcp
- rmalde: https://github.com/rmalde/minecraft-agent ; teknium1: https://github.com/teknium1/hermes-and-jev-play-minecraft
- souta-lab: https://github.com/souta-lab/minecraft-jev-agent
- Other Jev bots: https://github.com/Anahadd/jev-minecraft , https://github.com/RayB-007/jev-minecraft , https://github.com/ellistev/typesafe-minecraft-demo
- altoclef / ChatClef: https://github.com/gaucho-matrero/altoclef , https://github.com/MiranCZ/altoclef , https://github.com/elefant-ai/chatclef
- Companions: https://github.com/Goodbird-git/PlayerEngine , https://github.com/Goodbird-git/Player2NPC , https://github.com/adevivo/ai-companion
- Steve: https://github.com/YuvDwi/Steve
- Mindcraft-CE: https://github.com/mindcraft-ce/mindcraft-ce ; https://huggingface.co/Mindcraft-CE/Andy-4.2
- Real-client control: https://github.com/DRheEheAMGary/dsh-mc-client , https://github.com/0xjimmy/mineflayer-reflection-proxy , https://github.com/aesthetic0001/mineflayer-reflection-proxy
- Dreamer 4: https://arxiv.org/abs/2509.24527 ; https://danijar.com/project/dreamer4/
- HiSTEVE 1.0: https://arxiv.org/abs/2610.31162
- OpenHA: https://arxiv.org/abs/2509.13347 ; https://icml.cc/virtual/2026/poster/66679 ; https://icml.cc/virtual/2026/poster/66241 ; https://github.com/CraftJarvis/OpenHA ; https://github.com/CraftJarvis/MineStudio
- Optimus-3: https://arxiv.org/abs/2506.10357 ; https://github.com/JiuTian-VL/Optimus-3
- PUBG Ally: https://developer.nvidia.com/blog/how-krafton-built-pubg-ally-a-co-playable-character-powered-by-nvidia-ace/
- SIMA 2: https://deepmind.google/blog/sima-2-an-agent-that-plays-reasons-and-learns-with-you-in-virtual-3d-worlds/
- Project Sid: https://arxiv.org/abs/2411.00114 ; https://github.com/altera-al/project-sid
- ReplayStudio: https://github.com/ReplayMod/ReplayStudio
- PSN: https://arxiv.org/abs/2601.03509
- BotPrize: https://news.utexas.edu/2012/09/26/artificially-intelligent-game-bots-pass-the-turing-test-on-turings-centenary/
- Humanness detection: https://arxiv.org/abs/2105.09637 ; https://github.com/microsoft/NTT ; https://arxiv.org/abs/2604.09574 ; guardac.duckdns.org (vendor)
- Human input data: https://arxiv.org/abs/2206.11795 ; https://arxiv.org/abs/2505.12707
- MineWorld: https://arxiv.org/abs/2504.08388
