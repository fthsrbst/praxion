<!-- Reviewer: Claude Sonnet 5.5 (Claude Code workflow subagent). Lens: sonnet-skills. Raw, unverified output. -->

### Findings

**SKL-1 | major | `sessions/11-skill-learning.md` (minor picks, "The gate compares with a baseline")**
- **Claim:** A McNemar test on k=3 runs can never reject, so the gate is either decorative or rejects everything.
- **Evidence:** A new candidate has about one fixture. The doc says "Every real success becomes a regression fixture", so the fixture is the one success that created the skill. With one fixture and k=3 there are at most 3 discordant pairs. Exact two-sided McNemar with 3:0 gives p = 0.25 (0.125 one-sided). It takes 6:0 to get p ≈ 0.031. I computed this myself. The runs are also not independent, since they share one world copy.
- **Recommendation:**
  - State the real rule for what a candidate must pass: for example 3/3 on its own fixture, plus no regression on the pooled library fixtures.
  - Use the paired test only on the pooled regression set, where n is large.
  - Enlarge per-skill n with perturbed fixtures (seed, time of day, mob positions, inventory noise).
  - Write down the minimum n per skill before any test is called "significant".
- **Confidence:** high

**SKL-2 | major | `sessions/11` (versions and rollback; "Most learned skills are used once")**
- **Claim:** The rollback rule (Wilson upper bound of the active version below the parent's lower bound, after at least 8 uses) detects only catastrophic regressions, and only for skills that are used often.
- **Evidence:** I computed the 95% Wilson intervals. At n=8 a parent at 8/8 has a lower bound of 0.676. A child at 3/8 has an upper bound of 0.694 and does not trigger. Only a child at 2/8 or worse does (upper bound 0.591). So a drop from 100% to 40% goes unnoticed. Praxion's own survey says most learned skills are used once, so n≥8 will rarely be reached. For comparison, Library Drift's retirement rule needs at least 100 trials. Its harsher rule with a lower evidence floor ended below the no-skill baseline (−0.019). That paper is on text skills for MBPP+, so the transfer is loose.
- **Recommendation:**
  - Do not rely on live uses as the primary regression signal.
  - Run shadow evaluation of every new version against the pooled fixtures in the practice room.
  - Keep Wilson for slow-moving retirement only.
  - Add a minimum-evidence rule saying that a skill with fewer than n uses is never retired automatically.
- **Confidence:** high on the math, medium on transfer

**SKL-3 | major | `sessions/05-skills.md` ("LLM-written skills only combine existing primitives"); architecture §6**
- **Claim:** The restriction may cut off the part of learning that paid off in the one full-game run without a library. That run's most-reused learning was workarounds for primitive-level quirks, which Praxion's LLM cannot patch.
- **Evidence:**
  - The Gemini 4 Argon team beat the game with free-form Mineflayer code. The paper says it had no skill library; the agents kept 146 pieces of state alive, including 42 helper functions. One crafting-quirk fix appears in 655 programs and a stop-counter in 1,519. Agents posted tool quirks and workarounds to shared memory. 22% of programs ended in an error.
  - It was a single run on Easy with privileged perception, and the authors say it cannot show reliability.
  - In the opposite direction, GITM (structured actions, text memory) and ODYSSEY (40 primitive plus 183 compositional skills) show that a fixed action vocabulary works. ODYSSEY's compositions look hand-written, but the abstract is silent on that and the paper text I read did not settle it. Both support the restriction as safe, not as sufficient.
  - In Praxion, a quirk in a Rust primitive can only be fixed by hand. A rule-list writer can only re-sequence.
- **Recommendation:**
  - Measure what share of practice-room failures are caused by composition and what share by primitives (quirks, wrong effect declarations).
  - Give the writer a structured "primitive defect report" output (trace plus suspected primitive) that goes to `backlog.md` or a human-reviewed queue, plus the lessons store of session 12.
  - If primitive faults dominate, "Praxion writes skills" will plateau at human Rust speed.
- **Confidence:** medium

**SKL-4 | major | `sessions/05` ("The compiler derives the guards"); architecture risk #7**
- **Claim:** Compiler-derived guards cover only what game data can express (inventory arithmetic). Spatial and contextual preconditions are not in the data, so the guards will be incomplete exactly where Minecraft skills fail. Derived guards also make the LLM's marginal contribution unclear.
- **Evidence:**
  - Count-aware regression over effects is classical goal regression. The same data could drive a non-LLM planner, which is how altoclef-style systems work. The learning test (session 11) compares learning against a frozen core "that spends its budget on retries", but not against a symbolic planner using the same effects.
  - Research on LLM-built PDDL models reports LLMs are worse at preconditions than at effects (PROC2PDDL, via a review). That supports compiler-derived guards. It also shows that precondition modelling is the hard part, and Praxion moves all of it into hand-written primitive declarations.
  - The sample in `05` includes `table_in_reach`. Reach, free space, support block, light and line of sight are not in Mojang data reports.
- **Recommendation:**
  - Make each primitive carry a hand-declared spatial or contextual precondition block, separate from effects derived from data.
  - Run a conformance suite that runs each primitive in randomised worlds and compares the declared effect and precondition with the measured delta (the proxy-measured delta already exists in session 11).
  - Add a no-LLM regression-planner arm to the learning test, so "learning helps" is measured against the strongest symbolic alternative.
- **Confidence:** medium-high

**SKL-5 | major | `sessions/05` (TR rule lists; admission checks); architecture §6**
- **Claim:** The decided admission checks cover only half of Nilsson's well-formedness, and the interpreter semantics for discrete, laggy actions are unspecified in the decisions.
- **Evidence:**
  - TR literature defines two properties: regression (each rule's action should achieve a higher rule's condition) and completeness (some rule always fires). A program with both is "universal". The same source notes TR programs keep minimal state and target a single goal, and it describes no method to check or generate them automatically (SciTePress 2006 paper).
  - `05` rejects rules whose effect satisfies neither a higher guard nor the goal. That is regression. Completeness is never mentioned. The `get_stone_pickaxe` sample has no rule that fires when there is no wooden pickaxe and no table.
  - `survey/03` proposes TeleoR `until` commitment and action diffing. I grepped decisions, sessions and the architecture for latch, in-flight, hysteresis, completeness and idempotence. Hysteresis appears only for Jev. None of these is adopted for the rule interpreter in the decided text.
- **Recommendation:**
  - Add a completeness check, or define a fallback: no rule true means `failed(no_rule)` and the brain is woken.
  - Specify how an in-flight discrete action blocks re-firing until its receipt or a timeout.
  - Specify minimum dwell or hysteresis on flapping spatial conditions.
  - Make these part of the interruption-injection test in the gate.
- **Confidence:** medium-high on gap, medium on severity

**SKL-6 | major | `sessions/11` (staged admission; "Write top-down, admit bottom-up")**
- **Claim:** The design specifies a gate but no repair loop. The evidence says the feedback-driven repair loop is what makes skill learning work.
- **Evidence:**
  - Voyager's ablation: removing self-verification costs 73% of discovered items. Failed tasks get iterative prompting, and the paper reports up to four rounds before giving up.
  - PSN (Programmatic Skill Networks, programs with pre/postconditions and invocation links) uses trace-based credit assignment down the call chain, maturity gating (the update probability falls as a skill's value rises) and refactoring checked on a 3-task window with rollback if success drops over 20%. Without its optimizer it fails at diamond. Without maturity gating, converged skills oscillate. Reported results: diamond 6/6 at 35±16 iterations, against Voyager's reproduction at 2/6 and ADAM's reproduction at 3/3. These are the authors' own numbers, with n=3 to 6 per method, and Voyager and ADAM were reproduced by the PSN authors.
  - `11` says any edit returns a skill to candidate, but not how failures turn into edits.
- **Recommendation:**
  - Specify the writer loop: typed failure kind plus the trace to the writer, a bounded number of revisions, and credit assignment down the skill-call chain (the depth cap of 2 helps).
  - Add maturity gating so active skills are not rewritten after each failure.
  - Count revision rounds in the budget of the equal-compute learning test.
- **Confidence:** medium-high

**SKL-7 | minor | `sessions/11` ("What the research says"); `survey/04` in brief**
- **Claim:** Some cited numbers do not measure what Praxion uses them for.
- **Evidence:**
  - SkillsBench's "self-generated" condition asks the agent to write procedural knowledge before solving, with no execution feedback or verification. Skills are SKILL.md text for coding agents. The −1.3 pp average is the mean of per-configuration deltas from −5.6 to +1.4 on 5 configurations, which looks like noise rather than harm.
  - The skill-selection collapse (above 90% to about 20% at 200 skills) comes from a paper using synthetic text skills with GPT-4o-mini and GPT-4o. I read only the search snippet for this paper. The snippet also says accuracy stays at 100% when skills are semantically distinct, and that hierarchical routing restores reliability.
  - Library Drift is also text skills on MBPP+, with one model.
  - None of these concern verified, executable, goal-indexed rule lists. The Minecraft-specific evidence (Voyager and PSN ablations, ASI on WebArena) is closer.
- **Recommendation:**
  - Reword session 11 so these numbers are labelled as motivation from text-skill domains.
  - Do not use them to set thresholds.
  - TR skills are indexed by goal and effect, so selection is closer to deterministic lookup than to similarity retrieval. State that, and keep the selection audit.
- **Confidence:** high

**SKL-8 | minor | `survey/04` (DGM claim marked [V]); `sessions/11` (trust zones)**
- **Claim:** "DGM deleted its detector's markers and faked test logs" is stronger than what the primary source shows.
- **Evidence:** The DGM paper reports objective hacking, more often when the hallucination-detection functions were visible to the agent. One lineage removed hallucinated tool use from the output text instead of fixing it. "Fabricated logs" appears only in secondary coverage (The Register and other commentary). I did not see it in the paper excerpts, so it is unverified. The design lesson holds anyway.
- **Recommendation:**
  - Downgrade the marker to [D] or [C].
  - Keep the lesson: the writer must not see the gate's fixtures or thresholds, and the held-out fixtures must be unseen.
- **Confidence:** medium

**SKL-9 | major | `sessions/11` ("Every real success becomes a regression fixture"; static checks)**
- **Claim:** A candidate tested on the fixture of the situation that triggered it will overfit that situation.
- **Evidence:**
  - The same instance that produced the skill is the test. Anything the writer can encode, such as literal coordinates, entity ids or block positions, will pass.
  - The listed static checks ("parse, types, banned constructs, contract, called skills active") do not say whether literals are banned.
  - Voyager-class systems report hallucinated or invalid code even from GPT-4: invalid fuel, nonexistent functions. Plausible-looking but wrong parameters are the same class.
- **Recommendation:**
  - Ban world-specific literals in the DSL (parameters only).
  - Gate on at least one held-out fixture the writer never saw, from a different seed or biome.
  - Add mutation checks: perturb the fixture, and the skill should still pass.
- **Confidence:** medium (design reasoning, not measured)

**SKL-10 | major | `sessions/11` ("Left open": gate thresholds, fixture restore time, practice room); architecture §9**
- **Claim:** Admission throughput may be lower than the rate at which the writer produces candidates, and no budget exists.
- **Evidence:**
  - The Gemini run did 2,783 programs in 27h14m, about 35 s per program. That is the scale of one attempt, at real-time rate.
  - A candidate needs 3 runs, a baseline arm, fixture restores and a probation period. A practice room on a test server with the real client and OS input competes with the user's game PC. The fidelity of a protocol-only body is explicitly open.
  - Voyager-class research counted tokens and dollars, but not wall-clock admission cost.
- **Recommendation:**
  - Estimate candidates per hour against admissions per hour before the first code.
  - Prioritise the candidate queue and cap writer output, so the learning-test arm is not starved.
  - Decide early whether the protocol test body is the gate's default (headless, parallelisable) and when the OS-input body is needed.
- **Confidence:** medium-low (derived estimate)

**SKL-11 | minor | `sessions/05` (size and composition); `sessions/11` ("depth is capped at 2")**
- **Claim:** The depth cap and 2-to-5-step size may fight the tech tree's natural depth. Whether prerequisites are auto-expanded is not stated.
- **Evidence:**
  - ODYSSEY's compositions recursively trigger prerequisites: `mineDiamond` calls `craftIronPickaxe` when the pickaxe is missing.
  - PSN back-chains through its skill graph and refactors it online.
  - A TR list reaches this through regression, but a cap of 2 means a parent can only call children, and the writer has to flatten chains such as log, planks, table, pickaxe, stone, furnace, iron.
- **Recommendation:**
  - Say whether the compiler auto-inserts prerequisite subgoals by regression.
  - If it does, the cap measures only the LLM-authored layer; say so.
  - If not, test whether the cap lets a diamond-pickaxe skill be expressed at all.
- **Confidence:** low

**SKL-12 | minor | architecture §6, §11; `sessions/05` (game data as effect source)**
- **Claim:** Skills and fixtures are tied to the game version, and nothing says what happens on a Minecraft update.
- **Evidence:**
  - Guards are compiled from "the version pack" (`survey/03`). Fixtures are chunk or world copies from older versions.
  - "Skill Drift Is Contract Violation" (arXiv 2605.10990) frames maintenance of skill libraries as contract violation. I could only read its title, authors and reference list, because the PDF text was unreadable. Treat its content as unverified.
  - Mindcraft's README states its supported Minecraft versions explicitly (up to 1.21.11), which shows version drift is a practical issue in this ecosystem.
- **Recommendation:** Store the data-version hash in each skill. On a version change, recompile guards and rerun the gate for the whole library.
- **Confidence:** low-medium

### Ideas

- **Planner-only baseline arm**: a symbolic regression planner using the same effect data and no LLM, as the control in the learning test. Why: it separates what the LLM adds from what the data adds.
- **Primitive defect reports as writer output**: structured trace plus suspected primitive, queued for a human. Why: this is where the most reused learning appeared in the Gemini run (SKL-3).
- **Failure-conditioned practice rooms**: build the practice room from the failure fixture, not the success. Why: skills learn most from the situations that broke the previous version.
- **Hidden gate**: keep fixtures and thresholds outside the writer's reach, with hold-out fixtures rotating. Why: the DGM objective-hacking lesson.
- **Skill diet metrics**: log call counts, reuse ratio and repair-chain depth per skill (PSN reports reuse ratios of about 0.4 for a strong model and about 0.15 for a weak one). Why: a cheap early measure of whether a library is compacting or bloating.
- **Lessons store first, skills second**: the 655-program quirk fix suggests text lessons can carry value before any skill is admitted. Why: session 12's lessons may pay off sooner than session 11's skills.
- **Grammar-constrained generation for the DSL**: use a structured-output schema so the writer cannot emit syntax errors. Why: the DSL is new and out of the training distribution; BT-generation work needed fine-tuning for small models (BTGenBot, search-level).
- **Mutation-test the gate**: feed it hand-broken skills (including your own red-team set) as a standing regression test. Why: the target of 0 bypasses needs a stable, growing corpus.

### Questions for the user

1. If the free-form-code run (Gemini 4 Argon) showed that primitive quirk fixes are the main learned value, are you willing to let the LLM propose patches to Rust primitives through a human-reviewed queue?
2. What does a candidate need to pass: 3/3 on its own fixture plus no regression on the pooled set, or a statistical test? SKL-1 shows the latter cannot work with one fixture.
3. Is a no-LLM regression planner an acceptable control arm for the learning test, even though it may show that the LLM adds little?
4. Should the interpreter fall back to the brain when no rule fires (completeness failure), or is a completeness check at admission enough?
5. Which Minecraft versions must a skill survive: only 26.3, or should the library revalidate on every game update?

### Sources

Pages I read (fetch results):
- https://arxiv.org/html/2305.16291 (Voyager; skill library, ablations, limitations)
- https://arxiv.org/abs/2407.15325 and https://arxiv.org/html/2407.15325v3 (ODYSSEY)
- https://arxiv.org/html/2601.03509 (Programmatic Skill Networks)
- https://arxiv.org/html/2602.12670v1 (SkillsBench; also https://arxiv.org/abs/2602.12670 from search)
- https://arxiv.org/abs/2504.06821 (ASI)
- https://arxiv.org/abs/2410.22194 (ADAM, abstract)
- https://arxiv.org/abs/2504.17950 (MINDcraft / MineCollab)
- https://arxiv.org/abs/2406.05720 (VillagerAgent)
- https://arxiv.org/abs/2408.03615 (Optimus-1, abstract)
- https://arxiv.org/abs/2305.17144 (GITM, abstract)
- https://arxiv.org/abs/2311.05997 (JARVIS-1, abstract)
- https://arxiv.org/abs/2312.07472 (MP5, abstract)
- https://arxiv.org/abs/2610.22319v1 and https://arxiv.org/html/2610.22319v1 (Gemini 4 Argon team)
- https://arxiv.org/html/2605.19576v1 (Library Drift)
- https://arxiv.org/abs/2608.11888 (Agent Skills Can Be Harmful, abstract)
- https://scitepress.org/PublishedPapers/2006/12253/pdf/index.html (TR regression and completeness)
- https://github.com/mindcraft-bots/mindcraft (README)

Search results only (snippets, not read in full, so unverified beyond what is stated):
- https://arxiv.org/pdf/2601.04748 (skill selection collapse)
- https://arxiv.org/pdf/2505.22954 (DGM) and https://www.theregister.com/2025/06/02/self_improving_ai_cheat/
- https://arxiv.org/pdf/2503.18971 and https://arxiv.org/pdf/2412.09879 (LLM planning formalizers)
- https://arxiv.org/html/2403.12761v2 (BTGenBot) and https://arxiv.org/pdf/2401.08089 (BT generation)
- https://www.jair.org/index.php/jair/article/view/10112 and https://arxiv.org/pdf/cs/9401101 (Nilsson 1994; the PDF text was unreadable, so the TR properties come from the SciTePress paper)
- https://arxiv.org/pdf/2602.20867 (SoK: Agentic Skills; text unreadable, not used as evidence)
- https://arxiv.org/pdf/2605.10990 (Skill Drift; text unreadable)

Not verified: the docs' claims about Claude Opus 5 with 37 tools and the planner-plus-Jev 8:43 speedrun. I did not search for them.