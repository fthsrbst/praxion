<!-- Reviewer: DeepSeek v4.1 Flash via OpenCode opencode v2.1.2. Lens: deepseek-algorithms. Raw, unverified output. -->

I'll start by reading the core documents, then fan out across the session notes and research.
I've read the architecture, the decisions, all 15 session notes and the relevant research. Here is the review.

### Findings

**ID**: ALG-1
**Severity**: blocker
**Where**: `docs/architecture.md` §5 (perception), §6 (skills); `docs/decisions.md` 2026-10-29 "Seeing = line of sight…"
**Claim**: The `unknown` value never reaches the rule interpreter. Guards are boolean; nothing defines how a guard evaluates when a predicate is unknown, so in practice `unknown` degrades to `false` — the exact behaviour the perception decision forbids.
**Evidence**: §5 says "Out of sight a query answers `unknown`, not 'absent' **[P]**", and §6 says a skill is `condition -> action` with the "highest true rule" firing (session 5). In the canonical example (`docs/sessions/05-skills.md`), `has(wooden_pickaxe) -> mine(stone)` implicitly requires "stone in reach", which is unknown whenever no stone is in line of sight. Two-valued evaluation makes the skill fall to a lower rule that re-crafts a pickaxe — acting on absence. The guard compiler does not fix this: it makes guards *more precise*, not three-valued. Also missing: what happens when the top guard is unknown (probe? run the last stable rule? fail?).
**Recommendation**: Type guards as `{true, false, unknown}` with Kleene AND/OR (`unknown ∧ false = false`, `unknown ∨ true = true`). Declare every predicate with an observability class and a cost. Add `probe(P)` as a first-class primitive/action so unknown resolves by looking, and have admission reject any rule list where a guard can be unknown and no dominating probe exists.
**Confidence**: high

**ID**: ALG-2
**Severity**: major
**Where**: `docs/architecture.md` §6; `docs/sessions/05-skills.md`, `docs/sessions/07-reflexes-arbiter.md`
**Claim**: The rule language is under-specified to the point of being unimplementable: it has no `until`, no action diffing, no halt cleanup and no semantics for rule switches mid-primitive — yet the arbiter's margin rule depends on `until`.
**Evidence**: Session 7: "A switch inside one tier happens only when the gain exceeds `k x loss_on_interrupt` and **no `until` guard is active**". But `until` exists nowhere in the language (§6) or in session 5. `docs/research/survey/03` §1 ranks TeleoR's `until` and action diffing as the strongest thing to take, and warns that "releasing and re-pressing W can trigger a sprint" — with `SendInput` that is a real behaviour change. Undefined: what a durative primitive does when its rule stops being first (restart? continue to completion? abort?); what happens if no rule is true (stuck, fail, idle?); what happens when two consecutive rules select the same primitive with different arguments (`mine(stone@A)` → `mine(stone@B)`) — the dig-progress loss question is explicitly left open in survey/03; and the abort order for `do: skill(...)` when the parent's higher rule fires.
**Recommendation**: Write the interpreter semantics as a short formal spec with a truth table and a state diagram, then a semantics regression suite (BT.CPP's issue #755 is the cautionary tale). Add `until`/`while` commitment, action diffing keyed on (primitive, args), an `on_halt` hook per primitive, and an explicit "no rule true" outcome.
**Confidence**: high

**ID**: ALG-3
**Severity**: blocker
**Where**: `docs/architecture.md` §5 (action and verification); `docs/sessions/04-action-verification.md`; `docs/research/survey/01` idea 3
**Claim**: "Success needs the server's word" makes every movement task unverifiable, so the demo's own command suite ("go there", "follow me", "explore") can never be counted a success. The ladder is also wrongly ordered for digging.
**Evidence**: §5: "Without the server's word the result is `unconfirmed`", and session 4 rejects "the client agreed". But position is client-authoritative (`docs/sessions/02-body-connection.md` 2.1: "the client is authoritative for movement… the server may correct it with a teleport"), so the server's only word about movement is *not complaining*. Survey/01's own table already assigns look/move "the client's packets agree, and the server sends no correction" — i.e. `unrefuted`, a status the architecture never defines and no consumer uses. Second, for a durative dig the `block_changed_ack` acknowledges the client's sequence at/around the *start* of the dig, so "world changes succeed at level 2" reports success before the block is air.
**Recommendation**: Replace the single global ladder with a per-action evidence contract generated as data (dig → block is air; place → block state matches; pickup → inventory delta; move → unrefuted for N ticks with no `Player Position` correction; slot click → client packet + no correction). Add a receipt epoch (receipt tick > intention start tick) so a stale ack from a previous attempt cannot be counted. Define `unrefuted` as a first-class, consumable status.
**Confidence**: high

**ID**: ALG-4
**Severity**: blocker
**Where**: `docs/architecture.md` §4 (world model), §5 (perception); `docs/sessions/03-perception-world-model.md`
**Claim**: The world model as described — "keeps only what a player could see: line of sight in every direction within about 64 blocks" — is not computable inside any stated budget, has no algorithm, no update rule and no representation.
**Evidence**: A 64-block radius is ~1.1M cells (4/3π·64³). A generous DDA is ~100 steps per ray, so a full recompute is 10⁸ block reads; at 20 Hz (or inside the 5 ms state-update budget of E3) this is off by orders of magnitude. The doc gives no answer to: recompute per tick, per chunk load or lazily? What is stored per fact, when §4 promises "provenance on every fact" (1M facts)? Session 3 defers transparent blocks (glass, water, leaves) to "first code". And "everything else is denied **and written to the ledger**" would mean writing millions of denials per second. The policy also has no rule for data the client needs to render: the creeper's swell state and hurt flash are entity metadata, not F3 fields — deny them and the creeper reflex loses what a human sees; allow them and "everything else is denied" is false. Finally, entity visibility is bounded by the server's per-type tracking range, a second mechanism the single 64-block rule ignores.
**Recommendation**: Store what the decoder receives; make visibility a **query-time** filter (bounded DDA per candidate, cached per (target, eye-chunk), explicit ray budget per tick, "perception not ready" as an answer). Log *access attempts* with their verdict, not the denied facts. Decide the transparent-material table and the entity-tracking bound up front, and state the error model of the approximation (too short vs a human in distance, too wide in angle) since "zero cheats" is a headline claim.
**Confidence**: high

**ID**: ALG-5
**Severity**: blocker
**Where**: `docs/architecture.md` §4 (planner row), §11 risk 11; `docs/sessions/06-movement.md` ("Reuse: try to lift the A*, moves and cost code of Azalea")
**Claim**: The plan to lift Azalea's A*, moves and costs is based on capabilities Azalea does not have (bridging/placing) and on an executor model `SendInput` cannot reproduce (instant camera, atomic look-and-walk).
**Evidence**: I read the repository today (`main`; the `26.3` branch may differ — unverified). `pathfinder/moves/basic.rs` offers forward, ascend, descend, diagonal and downward only; `moves/uncommon.rs` adds one descend-forward move; there is no place/bridge/pillar move anywhere, and `BARITONE_COMPAT` explicitly disables moves Baritone has. Costs follow Baritone in ticks (`WALK_ONE_BLOCK_COST = 20/4.317`, `BLOCK_BREAK_ADDITIONAL_PENALTY = 2`), and mining cost comes from `MiningCache::new(inventory_menu)`. Executors emit `StartWalkEvent`, `LookAtEvent`, `StartSprintEvent` and call `look_at(target)` in the same tick they call `sprint()` — a protocol bot turns instantly. Praxion has one client tick per key change and 150 ms to aim within 1° (E6), so every node transition lags the plan by 1–4 ticks and the first ticks walk in the old facing. Baritone itself accepts weighted (non-admissible) A* with time-based cutoffs (`COEFFICIENTS`, `primaryTimeout`, `numEmptyChunk` cap), so "A*" here means bounded-best-effort, not optimal.
URLs: `https://github.com/azalea-rs/azalea/blob/main/azalea/src/pathfinder/moves/basic.rs`, `.../moves/uncommon.rs`, `.../pathfinder/mod.rs`, `.../pathfinder/costs.rs`, `https://github.com/cabaletta/baritone/blob/master/src/main/java/baritone/pathing/calc/AStarPathFinder.java`
**Recommendation**: Scope the reuse to "A* core + cost constants". Write place/bridge/pillar moves yourself (Baritone's `MovementTraverse`/`MovementPillar` as ideas only). Add a per-move `exec_error` term calibrated end-to-end on the real body (E9/E13), re-derive the cost table from measured ticks instead of Baritone constants, and make camera-then-move an explicit two-phase primitive (turn, settle, hold) with lag-tolerant `is_reached`. Drop parkour moves from rung 1.
**Confidence**: high

**ID**: ALG-6
**Severity**: major
**Where**: `docs/architecture.md` §4 (planner); `docs/sessions/06-movement.md` ("Policy object")
**Claim**: Inventory, tool tier and the ruleset are absent from the planning state, so the planner can produce paths it cannot execute and paths it is forbidden to execute, and only discovers this after paying the cost.
**Evidence**: The policy object lists "allowed moves, maximum drop, avoided blocks, exclusion areas, cost multipliers" — no inventory and no protected set. Azalea's own dig cost depends on the hotbar (`MiningCache::new(inventory_menu)`), and a bridge move costs one block per step. "May dig and bridge at a higher cost" implies a dig is always available; it is not without the right pickaxe (obsidian, ancient debris) or with a nearly-broken one.
**Recommendation**: Planning state = (position, hotbar/inventory summary, best tool per material, ruleset/protected set). Infeasible moves get cost ∞ in the planner, not a refusal at execution; if materials are missing, the plan must include an obtain sub-goal (see ALG-11). Encode "never dig straight down" and protected regions as move filters, not post-hoc checks.
**Confidence**: high

**ID**: ALG-7
**Severity**: major
**Where**: `docs/architecture.md` §4 (planner), §5 (perception); `docs/sessions/06-movement.md` ("plan to the frontier, look, plan again", "never dig into unknown blocks")
**Claim**: A* over a world that is mostly `unknown` is undefined in three places: frontier selection, what a dig exposes, and whether a bridge has support.
**Evidence**: With 64-block line of sight, a goal 500 blocks away is outside the visible region for most of the journey; "plan to the frontier" needs a policy for choosing among the ~10⁵ boundary cells (nearest? goal-directed? information gain?) and a heuristic that mixes goal distance with information — none is defined, and plain A* has no term for it. Second, "never dig into unknown blocks" checks the dug block, but the hazard is what the dig *exposes* (the neighbour behind the face may be lava or a water source). Third, a bridge move needs a known solid support at the landing cell; if that cell was never observed, its safety is unknown and there is no `probe` move to resolve it.
**Recommendation**: Define `safe_to_expose(face)` as a move precondition (all cells adjacent to the newly exposed face known and non-fluid); add `probe(target, ticks)` as a planner move with a cost; specify a cheap exploration policy (goal-directed frontier scoring with hysteresis, or a coarse long-range map from the chunk cache) as a separate component with its own tests. Mark void/bridge moves `unsafe_until`.
**Confidence**: high

**ID**: ALG-8
**Severity**: major
**Where**: `docs/architecture.md` §4 (arbiter); `docs/sessions/07-reflexes-arbiter.md` (tiers with two thresholds)
**Claim**: The arbiter's decision rule is not computable: `gain`, `loss_on_interrupt` and "next safe point" have no units, no estimators and no interface, and "hard threshold promotes to survival" conflates stateful needs with stateless reflexes.
**Evidence**: "A switch inside one tier happens only when the gain exceeds `k x loss_on_interrupt`" (session 7). Nothing defines gain (ticks of goal progress? risk? hunger points?), nothing estimates the loss from interrupting the running primitive except its cancellation class (`free`, `lossy`, `unsafe_until`, `compensable`), which is qualitative. The system's only quantitative cost unit is expected ticks (Baritone/Azalea). "The command's next safe point" is not defined per primitive, and a need promoted to "survival" must become a reflex, which session 7 defines as a pure function of the world — a need like sleep is not.
**Recommendation**: Make utility tick-denominated: gain = expected ticks saved toward the active goal; `loss_on_interrupt` = the running primitive's remaining estimated cost plus a per-class constant, exposed as `primitive.interrupt_cost()`. Define safe point = "the current primitive can be cancelled at cost ≤ its declared loss". Implement a hard threshold as a reflex predicate over the need state whose action is a maintenance primitive, so promotion is one mechanism, not two.
**Confidence**: high

**ID**: ALG-9
**Severity**: major
**Where**: `docs/architecture.md` §4 (reflexes, take-over); `docs/sessions/07-reflexes-arbiter.md` ("a reflex is a pure function"), `docs/sessions/04-action-verification.md` (`unsafe_until`)
**Claim**: The reflex layer cannot express its own required behaviours, and ownership is all-or-nothing, so a reflex that wants one key can destroy a danger-held key sequence.
**Evidence**: "A reflex is a pure function `evaluate(world) -> action`" (session 7) while the required set includes the water-bucket save, creeper evasion, skeleton strafing and "fight back" — all multi-tick sequences needing state (selected slot, current fall velocity, aimed pitch). Either the set shrinks to triggers or the purity claim is false. Ownership is "one owner of the body at a time, with epoch-stamped owner tokens"; a reflex that wants to eat takes the whole body, and nothing states that the held sneak of an `unsafe_until` bridge primitive survives that takeover. The same problem appears in session 4's take-over rule: releasing the held sneak when the user touches "any movement key" can itself walk the character off the edge — the safe behaviour is the opposite.
**Recommendation**: Separate the stateless trigger (≤5 ms predicate) from the stateful reactive primitive it selects (with its own cancellation class). Give the input layer a per-key lease table (~12 entries) with `danger_held` bits that no tier may revoke; `unsafe_until` becomes a property of the key lease. In a take-over, the danger-held key is the *last* thing released, never the first.
**Confidence**: high

**ID**: ALG-10
**Severity**: major
**Where**: `docs/architecture.md` §6; `docs/sessions/05-skills.md` (compiler derives the guards), §11 risk 7
**Claim**: "The compiler derives the conditions from the primitives' effects" presumes an effect algebra that does not exist, and effects alone cannot yield the preconditions the examples need. The artifacts are also inconsistent: the LLM writes *rules* in one sentence and *steps* in the next.
**Evidence**: Real effects are conditional and probabilistic — gravel→flint ~10%, leaves→sapling ~5%, fortune/silk-touch change drops, smelting consumes a variable fuel amount, `mine(x)` may drop into lava or expose water. Tool tier, reach and light are *preconditions*, not effects, so a compiler fed only effects cannot derive `has(stone_pickaxe)` from `mine(stone)`. §6 says "A skill is a teleo-reactive rule list `condition -> action`" and then "The LLM writes steps and a goal; the compiler derives the conditions" — if the LLM writes steps, the compiler must derive the *order* too (a topological sort over effects), not just the guards. Session 5's admission check ("rules whose effect satisfies neither a higher guard nor the goal") is a one-step regression test, not Nilsson's regression property.
**Recommendation**: Specify the effect record as a versioned data artifact first: `produces[(item, count, p)]`, `consumes`, `requires`, `changes[block@pos]`, `reveals[...]`, `duration_ticks`, `tool_tier`. Keep a separate precondition library from game data. Add an order-derivation step and a full regression + deadlock check to admission. State plainly that the graded artifact is the step program, and the rule list is its compilation.
**Confidence**: high

**ID**: ALG-11
**Severity**: major
**Where**: `docs/architecture.md` §6, §8; `docs/sessions/05-skills.md`, `docs/sessions/11-skill-learning.md`; missing component
**Claim**: The design routes the most repeated, exactly-solvable task family ("obtain X") through the LLM and the guard compiler, although the literature it already cites shows this family needs no LLM.
**Evidence**: `docs/research/systems-review.md` §3.5: Odyssey's `obtainItem` covers 789 items in ~1,265 lines of deterministic tables — "obtain X tasks need no LLM. A dependency graph derivable from minecraft-data plus a deterministic solver covers them." altoclef is 34k lines of hand-written task code for the same family (§3.7). Praxion instead makes "get a stone pickaxe" an LLM-written rule list whose correctness rests on a 3-run admission gate (ALG-13) and an unbuilt compiler (ALG-10). That inverts the cost/reliability curve at exactly the rungs the first demo needs.
**Recommendation**: Add a deterministic recipe/tech-tree solver under the TR runtime, sharing the effect/recipe data. Compile every `obtain(item, n)` goal to solver output; let the LLM write only situational skills (shelter siting, exploration, trading, combat positioning). This also makes the equal-compute learning test measure the LLM where it can actually add value.
**Confidence**: high

**ID**: ALG-12
**Severity**: major
**Where**: `docs/architecture.md` §4 (world model), §8 (memory); `docs/sessions/03-perception-world-model.md` (2-bit chunk cache, push invalidation)
**Claim**: Three stores of world knowledge (live body state, 2-bit body chunk cache, brain JSON Lines) have no single invalidation path; the cache for unloaded chunks is never invalidated, so the planner can plan through a wall the user built while the chunk was unloaded.
**Evidence**: Session 3: "Push invalidation … works only for chunks the client has loaded; for the rest, a fact stays stale until re-observed." Nothing reconciles the cache when a chunk is reloaded and fresh data contradicts it. Second, the Baritone 2-bit encoding (air/water/avoid/solid, ~24 KiB/chunk at 98,304 blocks) is a pathing-only structure, but it is also being used for remembered blocks, containers and the memory viewer — it cannot say *which* block (tool choice, protected-block checks, "where was that chest").
**Recommendation**: One authority in the body; brain memory is a projection over an append-only journal. On chunk load, diff and invalidate. Store remembered chunks as paletted snapshots with an LRU and a memory budget, and declare which fields each consumer may read. Define the clock (tick counter + session id) that "age" is measured on, since wall-clock ages break across restarts.
**Confidence**: high

**ID**: ALG-13
**Severity**: major
**Where**: `docs/architecture.md` §6, §8; `docs/sessions/11-skill-learning.md` (gate, rollback)
**Claim**: The admission and rollback statistics are underpowered and mutually inconsistent: a 3-run McNemar test can never reject at any conventional level, and 3 successes is a weak certificate for a library that grows to hundreds of skills.
**Evidence**: Session 11: k=3 paired runs with McNemar. With 3 pairs the exact two-sided p is at best 0.25 (all three discordant in one direction), so the test cannot reach 0.05 whatever the data. Three successes give a Wilson 95% lower bound of ≈0.44, and a skill that is broken but passes fixtures half the time still passes 3/3 with p=0.125 — over 100 candidates, ~12 broken skills are admitted by chance. §8 admits *lessons* on "three proxy-verified successful traces" while rollback waits for "at least 8 uses" and Wilson-bound comparisons: three different standards for one question.
**Recommendation**: One statistic everywhere. Use a sequential test (SPRT on paired fixtures) or a Clopper–Pearson/Wilson LCB with n≥12 and a required margin over baseline, under a pre-registered family-wise error budget (Bonferroni/BH) across the skill library. Version the fixture battery with each skill so results stay comparable.
**Confidence**: high

**ID**: ALG-14
**Severity**: minor
**Where**: `docs/architecture.md` §3 (time-scale table), §11 risk 3; `docs/sessions/13-natural-feel.md`, `docs/sessions/02-body-connection.md` 2.7 E3
**Claim**: Two different quantities are conflated under "50 ms": implementation latency and behavioural reaction time. The 5 ms budget is also stated for the whole perception pipeline, which chunk bursts cannot meet.
**Evidence**: §3 puts reflexes "under 50 ms"; session 13 fixes a "reaction floor about 150 ms" for naturalness. Both can be true only if the first is code latency, but the architecture never says so. Physically, the client reports position once per tick and entity updates arrive per tick (session 2/survey 03: "up to 50 ms of perception lag"), so sense-to-act is 0–50 ms of staleness + decode + one input tick even if the reflex code runs in microseconds. E3's pass criterion ("packet arrival to state update, p99 < 5 ms") cannot hold per packet during a join or dimension change, when hundreds of chunk packets arrive in a burst (E3 only says bursts are "reported apart"); and the architecture puts line-of-sight maintenance in the same pipeline.
**Recommendation**: State three numbers separately: (a) code latency per layer; (b) sense-to-act = staleness + decode + one client tick; (c) behavioural reaction time, a measured distribution ≥150 ms except lethal danger. Give per-packet-class budgets (play packet p99 5 ms; chunk throughput MB/s; burst policy; the answer "perception not ready" while a burst drains).
**Confidence**: high

**ID**: ALG-15
**Severity**: minor
**Where**: `docs/architecture.md` §4 (movement style); `docs/sessions/06-movement.md`, `docs/sessions/13-natural-feel.md`
**Claim**: A deliberate-deviation layer above the planner has no defined interface with the planner's tracking tolerance: the two documents specify opposite goals and no number bridges them.
**Evidence**: Session 6 wants "a movement-style layer (camera, speed, sprint timing, small deviations)" and lists "position error above a limit" as a replan trigger without a limit. Session 13 wants human-like aim statistics. Small lateral deviations break 1-block gaps and parkour; camera deviation during a precise dig breaks aim; and nothing says style is disabled inside `unsafe_until` actions or precision moves.
**Recommendation**: Restrict the style layer to domains that cannot perturb the path: head sway, look-ahead, sprint timing, pause distributions — never lateral position on precision moves. Define a style mode flag that is off inside `unsafe_until` and inside the last two blocks of any gap/jump move, and fix the tracking tolerance as a measured number that the replan trigger uses.
**Confidence**: medium

### Ideas (brainstorm)

1. **Query-time perception with a ray budget** — replaces a per-tick omni-directional visibility computation with bounded on-demand rays plus a cache; makes the fairness policy cheap, auditable and provable instead of aspirational (fixes ALG-4 and the ledger cost).
2. **One currency: expected ticks** — planner cost, arbiter gain, `loss_on_interrupt`, skill time estimates and settle windows all in ticks; removes the undefined units in ALG-8 and makes cross-layer comparison computable.
3. **Deterministic obtain solver, LLM for the situational remainder** — exact answers where exact answers exist; turns the learning story from "LLM writes a pickaxe recipe" into "LLM decides where to build and how to explore", which is what the literature actually supports (ALG-11).
4. **Per-key lease table with `danger_held` bits** — twelve entries and a bitmask; lets a reflex take only the keys it needs and makes `unsafe_until` survive every ownership change, including take-over (ALG-9).
5. **Predicate intern table + compiled guard DAG per tick** — evaluate each unique predicate once against one frozen snapshot, guards become boolean ops over interned ids; interpreter cost becomes `unique_predicates + guards`, a bound you can state and test rather than measure (ALG-2, ALG-14).
6. **One generated contract file per primitive** (effects, required evidence, cancellation class, cost, exec error) from the version pack — feeds the guard compiler, the receipt checker and the test matrix from a single source, so the three cannot drift (ALG-3, ALG-10).
7. **Copy-on-write planner snapshot with a world version** — Azalea plans over a live `RwLock<World>` (`CachedWorld`), so a path can be computed against a mutating world; a version-stamped snapshot turns that silent inconsistency into an explicit replan trigger (ALG-5, ALG-12).
8. **Reaction time as a sampled distribution, with a lethal-danger exception** — draw each non-lethal reaction from a measured human distribution and keep a fast path for creeper/lava/fall; reconciles session 7's 50 ms with session 13's 150 ms and lets the blind test and the reflex test measure the same quantity (ALG-14).

### Questions for the user

1. **Perception**: do you want to defend the exact 64-block omni-directional line of sight as the "fair" claim, or accept a cheaper, provable model (on-demand rays with a budget, shorter radius, entity tracking as a second bound) and document it as an approximation with a known error direction?
2. **Verification**: may movement and look count as verified when *unrefuted* (no server correction for N ticks), or must the demo's verifiable command suite be restricted to world changes and inventory effects? This changes what "a proxy-verified command suite" can mean.
3. **Skills**: is the "obtain X" family allowed to be deterministic code (a recipe solver) rather than LLM-written rule lists — i.e. is "writes its own skills" about situational skills only?
4. **Safety precedence**: when the user takes over mid-bridge, must Praxion keep the sneak and movement keys it holds until the moment passes (protecting the character), or yield every key the user touches as session 4 currently says?
5. **Demo scope**: does the first demo require long-distance travel (village/stronghold scale)? If yes, an exploration policy (frontier selection, coarse map) must be designed now, not after the first code; if no, it can be deferred to a later rung.

FINAL_REPORT The design's weakest point is the perception contract: "line of sight in every direction within 64 blocks" is neither given an algorithm nor a representation, and its output value `unknown` has no defined meaning in the rule interpreter, so the decision that out-of-sight data must not read as "absent" is silently lost at the first guard evaluation (ALG-1, ALG-4). Equally fundamental is that the receipt ladder cannot verify the actions the demo depends on: position is client-authoritative, so movement can never reach "server confirmed", and a global rule of "success needs the server's word" would mark the command suite unverifiable while a level-2 ack would mark a durative dig as successful before the block breaks (ALG-3). Third, the planner is scheduled to be lifted from Azalea, whose move set contains digging but no placing or bridging and whose executors rotate the camera instantly, while Praxion's hands add a tick of key latency and a 150 ms aim budget — so the cost model, the move set and the executor all need to be re-derived rather than ported (ALG-5), with the arbiter's undefined gain/loss units (ALG-8) and the missing effect algebra behind the guard compiler (ALG-10) as the next tier of blockers.
