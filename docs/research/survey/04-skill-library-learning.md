# Skill and library learning beyond Minecraft

Research date: 2026-10-25. Markers: [V] verified by an independent check, [C] corrected after checking, [D] documented or vendor claim, [U] unverifiable.

Scope: how agents outside Minecraft induce, verify, maintain and retire reusable skills, and what that implies for a Praxion skill admission pipeline. Benchmark numbers are self-reported by the authors unless stated otherwise.

## In brief

- **Writing a skill is cheap; deciding which skills to keep is the real work.**
  - ASI (WebArena shopping): unverified text skills 32.6%, verified programs kept in memory 36.4%, verified programs used as actions 40.1% [V].
  - SkillsBench v1: curated skills +16.2 pp; skills a model writes for itself without checking -1.3 pp [C].
- **Strong gates run the skill and compare it with a baseline on the same tasks.** ASI re-runs the source task. SkillGen counts repairs minus regressions against cached no-skill outcomes [V]. Gates that rely on an LLM judge can be gamed: DGM deleted its detector's markers and faked test logs [V].
- **Judge a skill per context, not by one global score.** Assay found that over 90% of GPT-5.1's skills help on some tasks and hurt on others. Its rule: try splitting a skill before retiring it [C].
- **Choosing among skills gets much worse as the library grows.** GPT-4o-class models choose correctly over 90% of the time from 20 skills or fewer, but only about 20% of the time from 200. Look-alike skills cost 17-63% accuracy [V]. Jev has never been measured.
- **Claims that learned skills get reused are weak.** Most learned functions are called only once, and TroVE gains about 1% once compute is matched [V]. "Praxion learns skills" therefore needs reuse statistics and a no-library comparison run on the same LLM budget.
- **Find the faulty skill from structured traces, not by asking an LLM to read logs.** LLMs reading logs find the failing step only 14.2% of the time (Who&When) [V]. PSN instead traces blame from parent to child along the execution trace and applies fixes from children up [C].
- **Code skills are for what can be executed and checked; text lessons hold conditions and warnings.** Change both through small edits, never a full LLM rewrite (ACE, ExpeL) [V].

## Key mechanisms

### 1. Admission by re-execution (ASI) [V]

```
successful episode -> clean trace -> LLM writes skill functions (2-5 primitive steps, no try/except, with docstring)
                                     and rewrites the episode to use them
back up library -> append functions -> replay rewritten trace, CUT after last skill call -> agent finishes task
admit only if: (a) task judged solved, (b) >=1 step calls a new skill, (c) every skill call was valid and changed state
otherwise: restore backup
```

Cutting the replay after the last skill call means a trailing primitive action, such as sending a message, cannot fake a pass. Admitted skills become callable actions. Induction succeeded on 55 of 725 episodes; 308 of 725 reused a skill. On transfer, the paper's text says 10 queries per site pair, but its Table 6 implies 30.

### 2. Repair-versus-regression gate (SkillGen) [V]

Instances are split 70/30 into induction and verification, with at least 4 for verification. The no-skill outcomes are cached. For up to K=8 rounds, each candidate runs on the m verification instances:

```
repair = 0 -> 1     regression = 1 -> 0     G_m = repairs - regressions
active only if G_m >= max{2, ceil(0.05*m), 1}, plus up to 30 guard reruns on instances the baseline solved
otherwise: 'deprecated', replaced by the empty skill
```

Bad skills still got through. One accepted skill dropped Mistral-Nemo on ChemLLMBench from 43.33% to 20.00%. The gate also assumes deterministic outcomes, which Minecraft does not have.

### 3. Per-context attribution by masking (Assay) [C]

Assay draws K=12 masks; in each, a skill is kept with probability f=0.4. For skill j on task i, C[j,i] is the mean outcome with j minus the mean without it. From these it computes a global effect Cbar(j) (row mean) and a heterogeneity H(j) (row range).

Repair runs in a fixed order:
1. Split skills with H >= 0.40 into conditional variants.
2. Retire skills with |Cbar| < 0.10.
3. Merge pairs with embedding cosine >= 0.85.

The rewritten library is accepted only if its pass rate is at least the original's on all 15 development tasks. Splitting runs first so that a skill whose effects cancel out is not retired. At inference, skills predicted below -0.10 from the 8 nearest development tasks are suppressed.

Result on AppWorld: DeepSeek-V3 47.0 to 69.3. The ACE library alone scored 49.9, below plain ReAct at 52.5.

Correction: the linked repository returned 404 on 2026-10-25; the code is not released.

### 4. Skill network: blame, maturity gate, rollback (PSN v2) [C]

- **Skill structure.** Each skill has control flow, parameters, preconditions and postconditions, and child skills. Every call is logged with the state before and after.
- **Value.** V(s) = p_hat - u. p_hat is the Laplace-smoothed success rate. u is an uncertainty term; the paper does not give its formula.
- **Repair order.** Blame runs top-down along the trace. Edits are applied bottom-up, children first. A buffer of the 5 most recent proposals prevents contradictory edits.
- **Update gate.** P(update) = (1-eps)*sigmoid(5*(0.6-V)) + eps, with eps = 0.1. **Correction:** the floor is about 0.21, so a mature skill is still rewritten on roughly 1 in 5 failures it is blamed for. Mature skills are not "rarely touched".
- **Rollback.** If success over the 3 most recent related tasks drops more than 20%, the change is undone. The paper does not say whether that drop is absolute or relative.
- **Corrections.** Code is released (MIT, active). The bug-rate drop from 65% to 35% comes from the whole gate (checker plus API-contract docs), not from the static checker alone.

### 5. Gated refactoring and documentation (ReGAL, LILO) [V]

- **ReGAL.** Each helper function keeps the programs that call it as test cases. A refactor is kept only if strictly more of those cases pass; otherwise it is backed out. A helper is kept if it is used at least 4 times, or is under 40 rounds old.
- **LILO.** Stitch compresses the code, then AutoDoc has an LLM name and document each new abstraction. Undocumented abstractions cost the LLM solver 30.6 points on REGEX. When GPT-4 was prompted to compress code itself, it reached about 1.0x, and only 19% of its outputs were valid.

### 6. Lineage and promotion (DGM, SICA) [V]

- **DGM** stores each agent as a chain of patches, so lineage and rollback come free. It evaluates in stages: a small subset first, the full set only for promising agents.
- **SICA** promotes the newest version whose mean is at least the best version's lower confidence bound: prefer fresh code unless it is statistically worse.

## Landscape

| Work | Kind | Date | What it shows | Availability | Marker |
|---|---|---|---|---|---|
| ASI | paper, COLM 2025 | 2025-04 | Verified programs used as actions: 40.4%, vs AWM 36.3% and vanilla 32.7% | CC BY-SA 4.0, inactive | [V] |
| SkillGen | paper | 2026-05 | Paired gate; +3.27 to +10.08 pp; 5 of 80 entries regressed | yccm/SkillGen, Apache-2.0 | [V] |
| Assay | paper | 2026-06 | Split, then retire, then merge; AppWorld 47.0 to 69.3 | code link 404 | [C] |
| PSN v2 | paper | 2026-06 | Diamond in 35+/-16 iterations (6/6 runs) vs Voyager* 99+/-36 (2/6) | MIT, active | [C] |
| SkillWeaver | paper | 2025-04 | Test count resets on every edit; GPT-4o 22.6 to 29.8 | MIT | [V] |
| SkillsBench | benchmark | 2026-02 | Curated skills help; unchecked self-written ones do not; best with 2-3 focused skills | Apache-2.0 | [C] |
| Library Learning Doesn't | paper | 2024-10, 2025-04 | Learned functions rarely reused; gains come from self-consistency | public | [V] |
| TroVE + re-evaluation | paper | 2024-01, 2025-07 | About 1% gain once compute is matched | CC BY-SA 4.0 | [V] |
| ReGAL | paper, ICML 2024 | 2024-01 | Refactor kept only if more tests pass; +8 to +26 points | no licence | [V] |
| Stitch | tool, POPL 2023 | 2022-11 | 3-4 orders of magnitude faster than DreamCoder | MIT | [V] |
| LILO | paper, ICLR 2024 | 2023-10 | Undocumented abstractions hurt (-30.6 points) | no licence | [V] |
| DGM / HGM | paper, ICLR 2026 | 2025-05, 2025-10 | Patch lineage; staged evaluation; hacked its own verifier | Apache-2.0 | [V] |
| SICA | paper, workshop | 2025-04 | Promotion by confidence bound; utility includes cost and time | MIT | [V] |
| AlphaEvolve / OpenEvolve / ShinkaEvolve | technique | 2025-06 | Evaluation cascade; novelty rejection | AlphaEvolve closed; others Apache-2.0 | [V] |
| SkillSmith | paper | 2026-05 | Epitaphs veto repeats of failed ideas | no code | [C] |
| EvoSkill | paper | 2026-03 | Git-branch frontier of the 3 best programs; single runs only | Apache-2.0 | [V] |
| ACE | paper, ICLR 2026 | 2025-10 | Counters with small edits; its library scored below ReAct in Assay | Apache-2.0 | [V] |
| ExpeL | paper, AAAI 2024 | 2023-08 | Vote arithmetic that knows the list's capacity | Apache-2.0 | [V] |
| AutoManual | paper, NeurIPS 2024 | 2024-05 | Typed rules compiled into a readable manual | no licence | [V] |
| AWM | paper | 2024-09 | Text workflows from successful runs | Apache-2.0 | [V] |
| Skill-selection phase transition | paper | 2026-01 | Selection degrades past about 84-92 skills; hierarchy restores 72-85% | public | [V] |
| Who&When | benchmark, ICML 2025 | 2025-05 | LLMs find the failing step only 14.2% of the time | public | [V] |
| REFACTOR-VLA / LRLL | paper | 2026-09, 2024-06 | Two skills are equal if their behaviour matches; MDL gate | public | [V] |
| Code as Policies | paper | 2022-09 | Undefined functions are written recursively | public | [V] |
| DreamCoder / babble | paper | 2020-06, 2022-12 | Wake-sleep library learning; e-graphs | public | [V] |
| CRAFT, LATM, Alita(-G) | paper | 2023-2025 | Agents that create tools; a strong model makes them, a cheap one uses them | public | [V] |
| SoK, Dynamic Agent Skills, Verifiable Artifacts, EvoSkill Injection | surveys | 2026 | Lifecycle states, trust tiers, skill-injection attacks | public | [V] |
| Anthropic Agent Skills | doc | accessed 2026-10-25 | Staged loading: about 100-token metadata, body under 5k, scripts executed | open | [V] |
| Dynamic Cheatsheet, ReasoningBank, GEPA | papers | 2025 | Adjacent work on lesson stores and text evolution | not read | [U] |

Version notes:
- **SkillsBench.** The numbers above are from v1. v4 reports 87 tasks, 18 configurations, and a rise from 33.9% to 50.5% with curated skills (+16.6 pp). Its "NeurIPS 2026" status is unverified.
- **SkillSmith.** The numbers above are from v1 (OfficeQA 61.8 to 80.1). v2 reports 74.1 to 96.1, renames epitaphs to "nogood learning", and retires skills probabilistically.
- **SICA.** On SWE-bench, 53% is the peak at iteration 14; the final iteration scored 0.51.

## What Praxion can take

Ranked strongest first. Session numbers refer to `docs/discussion-plan.md`.

1. **ASI's admission test, with "changed state" replaced by "matches the declared effect"** (sessions 5, 11, 14).
   - Re-run the task with the new skill, require that the new skill is called, and cut the replay after the last skill call.
   - "Changed state" is the wrong test for skills such as wait, sneak or query, which may correctly change nothing.
   - Make skills callable actions, as survey/02's TR rule DSL already does.
2. **Reuse statistics plus a no-library comparison at the same compute** (sessions 14, 15). Track calls per skill, the share of skills used once, and how many composites call each skill. These cost nothing to log, and no current doc requires them.
3. **PSN's structure** (sessions 5, 6, 11): state snapshots on every call, and blame from parent to child with fixes applied from children up. A rewrite allowed by the maturity gate still enters as a candidate through the gate. PSN's 3-task rollback window is too small for a random game.
4. **The skill writer never edits its judges** (sessions 11, 14, 15). Keep skill versions as a patch or git lineage, so rollback is a pointer swap (DGM).
5. **SkillGen's paired counting, made robust to randomness** (sessions 11, 14).
   - Run each fixture k times.
   - Apply an exact McNemar test to the fixtures whose result changed between versions.
   - Require zero regressions only on guard fixtures that pass reliably (3 of 3 replays).
6. **Never show an undocumented abstraction to the planner** (LILO; sessions 5, 11, 12). A name and a "when to use / when not to" note are part of admission. Do not let an LLM compress the library from scratch.
7. **A refactor may not lose a passing fixture** (ReGAL; sessions 5, 11).
8. **Any edit resets the test count; untested versions stay hidden** (SkillWeaver; sessions 10, 11). Add "no catch-all try" to the static bans the docs already list.
9. **Promote by lower confidence bound, with cost and latency in a skill's utility** (SICA; sessions 11, 14). Skip SICA's LLM overseer; survey/01 chose receipts plus offline audit.
10. **Consider splitting before retiring** (Assay; sessions 11, 12, 14). Log a context key on every call now; postpone the masking studies.
11. **Epitaphs as the single store of negative memory** (SkillSmith; sessions 7, 11, 12).
12. **Lifecycle states with provenance, and tags for each skill's origin** (surveys; sessions 11, 14, 15). Origins are curated, induced from play, or taught through chat. Skills from chat pass the same gate.
13. **Rules for the lesson store** (sessions 8, 9, 12): small edits with counters (ACE, ExpeL), and typed rules compiled into a manual the user can read (AutoManual).
14. **Measure how many options Jev can choose among, and audit look-alike skills** (sessions 8, 12).
15. **Write composite skills top-down, then generate and gate each child bottom-up, with a depth cap** (Code as Policies; sessions 5, 11).

Weak, not adopted:
- **TroVE's trim rule** is arbitrary.
- **Stitch and DreamCoder-style sleep** need a DSL and a large library. Keep them as an argument in session 5.
- **AlphaEvolve-style search** is infeasible on one PC in real time.
- **EvoSkill** duplicates SkillGen with a weaker rule.
- **REFACTOR-VLA** counts only as precedent.
- **LATM's maker/user split** is already implied by recorded decisions.
- **Alita** creates tools without a gate, which contradicts the admission rules above.

## Ideas for Praxion

**1. Skill CI: staged admission (strong).**
- **Mechanism (architect's sharper version).**
  - Three stages: static checks (parse, types, banned constructs, contract, called skills active); harness fixtures run k=3 times with a McNemar test; then probation on the harness server only.
  - Skill states are candidate, probation, active and retired. Quarantined and deprecated are flags. Any edit returns a skill to candidate.
  - The curated core passes the same gate first.
  - Add a selection-regression stage only once option counts exceed Jev's measured limit.
- **Test.** Feed in 50 mutants (no-op, wrong count, side effect, protected-block write) and 50 good refactors. Require 0 mutants admitted and at most 10% of good refactors rejected. Report wall-clock time per admitted skill.
- **Risk.** Real-time stages are slow. A protocol test body cannot catch motor-layer failures. A strict gate can stop learning; ASI admitted from only 7.6% of episodes.
- **Sessions.** 5, 11, 14, 15.
- **Architect's note.** The original six-stage version is "too big to build in dependency order".

**2. Effects measured from the proxy, and duplicates found by behaviour (strong).**
- **Mechanism.**
  - For each call, framework code computes a typed delta: inventory, blocks, position and dimension, entities, health and hunger, time.
  - It checks the delta against the skill's declared schema and returns one of: success, partial, wrong-effect, no-op, side-effect violation.
  - Sharper version: attribute each block change to Praxion's own dig and place packets through sequence numbers and acknowledgements. Count unattributed changes as environment.
  - Generate schemas from recipe and loot data where possible; an LLM writes the rest.
  - Treat two skills as duplicates if their deltas agree on at least 95% of shared fixtures.
- **Test.** Count successes the skill reported that break its schema, over 200 logged runs. Compare behavioural deduplication with embedding cosine (0.85 and 0.90) on 30 labelled pairs.
- **Risk.** The proxy cannot see some effects. Session 3 must decide whether the verifier may read data that perception hides from the agent.
- **Sessions.** 3, 4, 5, 11.

**3. Trust zones and mutation-tested verifiers (strong).**
- **Mechanism.** The skill writer is a separate process that can write only to `skills/candidates/`, and any diff outside it is rejected. Only framework code computes postconditions. Evidence logs are append-only.
- **Test.** A red-team LLM tries to get 20 broken skills admitted, including through chat-based injection. Target: 0 bypasses. A weekly mutant zoo checks the gate's health.
- **Risk.** Effect schemas may be too permissive. Process isolation on Windows takes real engineering.
- **Sessions.** 11, 14, 15.

**4. A learning test at equal compute, plus a library health dashboard (strong).**
- **Mechanism.**
  - Two arms at equal tokens per hour: learning on, and a frozen curated core that spends its budget on retries.
  - At least 20 paired seeds on rungs 1-3, with the analysis pre-registered.
  - Report rung-reach difference with confidence intervals, single-use ratio, fan-in, gate statistics and hosted cost per hour.
- **Test.** The protocol is itself the test.
- **Risk.** It is expensive, and the gain may be zero; that is still a valid finding. Keep this claim separate from the flawless-demo claim.
- **Sessions.** 14, 15.

**5. Every real success becomes a regression fixture (useful).**
- **Mechanism (sharper).** Copy the touched chunks or the world folder, plus the player's NBT, goal and effect signature. Restore by stopping the test server, copying the files and restarting it; never restore in the user's world. A new version must keep every fixture its predecessor passed.
- **Test.** Measure restore time first: above 30 s, fixtures run as a nightly batch. Then replay 100 fixtures 5 times each and set k from the reproduction rate.
- **Risk.** Entity state is lost on restore. `/tick freeze` does not freeze players, and how the 26.3 client behaves under `/tick` is unverified.
- **Sessions.** 11, 14, 15.

**6. Lineage with automatic rollback (useful).**
- **Mechanism (sharper).** Keep the versions in git, with one active pointer per skill family. Promote by SICA's confidence-bound rule. Roll back when the active version's Wilson upper bound falls below its parent's lower bound, after at least 8 uses. Skip HGM and DGM parent selection.
- **Test.** An injected degraded version must be rolled back within 10 uses, with no false rollbacks across 20 healthy versions.
- **Risk.** Rare contexts produce too few samples; rely on fixtures there.
- **Sessions.** 11, 14.

**7. Two stores with a promotion path (useful).**
- **Mechanism (sharper).** Text lessons carry counters and go only to the LLM, never into Jev's state, because state text steers Jev. A lesson is promoted to code induction after at least 3 agreements and at least 2 matching successful traces verified by the proxy. Retired skills leave epitaphs.
- **Test.** Compare lessons only, skills only, and both, on rungs 1-3 over 20 or more paired seeds.
- **Risk.** Lessons judged by an LLM can be wrong and persist; keep each lesson's evidence with it.
- **Sessions.** 8, 11, 12.

**8. Look-alike audit for Jev (useful).**
- **Mechanism (sharper).**
  - Measure how many options remain after code filtering in logged play, and Jev's accuracy and calibration at 5 to 80 options, with and without look-alikes.
  - Add a category level only if the 95th-percentile option count exceeds the measured knee.
  - A candidate whose card has cosine >= 0.85 to an existing card must not lower Jev's accuracy on 100 held-out items.
- **Test.** As in the mechanism.
- **Risk.** A category step adds 130-300 ms on the critical path.
- **Sessions.** 8, 9, 12.

**9. Write top-down, admit bottom-up (useful).**
- **Mechanism (sharper).** The parent is written against children that do not exist yet. Each child is 2-5 steps, has its own fixture, and is gated first. Depth is capped at 2. A child still used only by its parent after 14 days is merged back into it.
- **Test.** On 10 composite goals, compare with whole-function generation: admission rate on the first try, LLM calls, and mature skills modified (target 0).
- **Risk.** Fixtures multiply, and leaves may be used once.
- **Sessions.** 5, 10, 11.

**10. Retirement that accounts for context (the mechanism is weak; the rule change is kept).**
- **Mechanism (sharper).** Retire a skill only if its Wilson upper bound is below the threshold in every context with at least 8 events, and its failures are typed "skill broken" rather than "world unsuitable". Otherwise flag it for a split. Run masking only once there are at least 40 non-core skills, and only on harness seeds.
- **Test.** Replay logs; compare against a single global Wilson rule.
- **Risk.** Context data will be sparse.
- **Sessions.** 11, 12, 14.

Weak ideas:
- **Nightly Stitch.** Build it only if more than 15% of skill code is shared structure, once there are 40 or more skills.
- **Practice.** Only on the harness server, never in the user's world (side effects, naturalness, session 13).

## Challenges to recorded decisions

1. **"LLMs may write skills" and the premise "writes its own skills while it plays."** The challenge holds as a constraint, not as a reversal. SkillsBench's -1.3 pp comes from text skills that were never executed, and ACE is a text playbook. Both argue for gating, not against executable skills checked by the proxy; ASI shows verified programs help. Consequences:
   - The first demo rests on the curated core.
   - LLM skills enter only through the gate.
   - "It learns" needs the comparison at equal compute.
2. **PSN numbers.** The challenge holds.
   - game-agents.md cites v1 (diamond 51+/-9); that figure does not appear in v2.
   - v2 Table 1: PSN gpt-5-mini 35+/-16 (6/6); Qwen3-Coder-Next 49+/-18 (6/6); Voyager* 99+/-36 (2/6); original Voyager 102 (1/3); ADAM* 74+/-23 (3/3); without optimizer 0/3.
   - survey/01 line 166 ("PSN v2: no new numbers, no code") is wrong on both counts.
3. **The gap in effect declarations (systems-review).** The challenge holds, restated. The open gap is typed effect schemas that framework code checks against what the proxy observes, independent of what the skill reports. Attribute effects through sequence acknowledgements.
4. **Retirement on a Wilson bound alone (Craftonomous).** The challenge holds in part. The failure mode is real, but Craftonomous already keeps per-context statistics, and Assay's evidence comes from text skills. The minimal fix is idea 10.
5. **Jev picking among all skills in one flat Choice.** This does not hold as stated: decision-models.md already has code list the legal options first. What remains is that Jev has never been measured on option count or look-alike skills (idea 8).

## Open questions

- How faithfully does a protocol test body stand in for the OS-input body? (sessions 2, 4, 14)
- How deterministic and how fast are fixture restores done by copying files? This decides between k=1 and k=3, and between gating each candidate and a nightly batch.
- Skill language: free TypeScript or Python, a restricted subset, or a tree DSL? (session 5)
- Who writes effect schemas that game data cannot generate, and how is a schema that is too weak detected?
- How accurate and well calibrated is Jev with 5 to 80 options and with look-alike skills?
- Which gate thresholds fit real-time Minecraft: m, the number of guards, the McNemar alpha?
- When the game version changes, are skills re-gated, and how are fixtures migrated?
- Skill granularity: 2-5 steps (ASI), compact skills (SkillsBench), or deep graphs (PSN)?
- How are skills taught through chat secured against injection (EvoSkill Injection)?
- Do Dynamic Cheatsheet, ReasoningBank or GEPA change the design of the lesson store? Not read yet.

Dropped after checking: none. The fact-checker refuted no item; its corrections are applied above.

## Sources

- https://arxiv.org/abs/2504.06821 ; https://github.com/zorazrw/agent-skill-induction
- https://arxiv.org/abs/2605.10999 ; https://github.com/yccm/SkillGen
- https://arxiv.org/abs/2606.15390 ; https://github.com/aiming-lab/assay (404 on 2026-10-25)
- https://arxiv.org/abs/2601.03509 ; https://github.com/evolving-skill-networks/psn
- https://arxiv.org/abs/2504.07079 ; https://github.com/osu-nlp-group/SkillWeaver
- https://arxiv.org/abs/2602.12670 ; https://github.com/benchflow-ai/skillsbench
- https://arxiv.org/abs/2410.20274 ; https://arxiv.org/abs/2504.03048
- https://arxiv.org/abs/2401.12869 ; https://arxiv.org/abs/2507.22069 ; https://github.com/zorazrw/trove
- https://arxiv.org/abs/2401.16467 ; https://github.com/esteng/regal_program_learning
- https://github.com/mlb2251/stitch
- https://arxiv.org/abs/2310.19791 ; https://github.com/gabegrand/lilo
- https://arxiv.org/abs/2505.22954 ; https://github.com/jennyzzt/dgm ; https://arxiv.org/abs/2510.21614 ; https://github.com/metauto-ai/HGM
- https://arxiv.org/abs/2504.15228 ; https://github.com/MaximeRobeyns/self_improving_coding_agent
- https://arxiv.org/abs/2506.13131 ; https://github.com/algorithmicsuperintelligence/openevolve ; https://github.com/SakanaAI/ShinkaEvolve
- https://arxiv.org/abs/2606.01314
- https://arxiv.org/abs/2603.02766 ; https://github.com/sentient-agi/EvoSkill
- https://arxiv.org/abs/2510.04618 ; https://github.com/ace-agent/ace
- https://arxiv.org/abs/2308.10144 ; https://github.com/LeapLabTHU/ExpeL
- https://arxiv.org/abs/2405.16247 ; https://github.com/minghchen/automanual
- https://arxiv.org/abs/2409.07429 ; https://github.com/zorazrw/agent-workflow-memory
- https://arxiv.org/abs/2601.04748
- https://arxiv.org/abs/2505.00212 ; https://github.com/ag2ai/Agents_Failure_Attribution
- https://arxiv.org/abs/2609.01215 ; https://arxiv.org/abs/2406.18746
- https://arxiv.org/abs/2209.07753
- https://arxiv.org/abs/2006.08381 ; https://arxiv.org/abs/2212.04596
- https://arxiv.org/abs/2309.17428 ; https://arxiv.org/abs/2305.17126 ; https://arxiv.org/abs/2505.20286 ; https://arxiv.org/abs/2510.23601
- https://arxiv.org/abs/2602.20867 ; https://arxiv.org/abs/2607.10113 ; https://arxiv.org/abs/2605.00424 ; https://arxiv.org/abs/2608.30429
- https://platform.claude.com/docs/en/agents-and-tools/agent-skills/overview
- https://arxiv.org/abs/2504.07952 ; https://arxiv.org/abs/2509.25140 ; https://arxiv.org/abs/2507.19457
