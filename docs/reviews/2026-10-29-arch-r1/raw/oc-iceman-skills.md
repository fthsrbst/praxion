<!-- Reviewer: Iceman Contributor Free via OpenCode opencode v2.1.2. Lens: iceman-skills. Raw, unverified output. -->

### Findings

- **ID**: SKILL-1  
  **Severity**: blocker  
  **Where**: `docs/sessions/11-skill-learning.md` §Admission, `docs/architecture.md` §6  
  **Claim**: The admission gate uses k=3 runs against a baseline in the practice room. For stochastic Minecraft tasks (mob spawns, drops, pathing, block updates), n=3 is statistically underpowered and cannot reliably distinguish a skill that repairs cases from one that just got lucky.  
  **Evidence**: Session 14 requires "many fresh seeds, confidence intervals, analysis written before the runs" as the evidence standard, yet skill admission only specifies k=3 with a McNemar comparison. The research notes in Session 11 also state "most learned skills are used once" which makes false positives expensive if they pollute the library.  
  **Recommendation**: Do a power analysis per task class and require a minimum number of seeds (or fixtures) for admission. At minimum, require confidence intervals on the repair/break counts and block admission when the interval crosses zero. Treat n=3 as a smoke test only, not a gate.  
  **Confidence**: high

- **ID**: SKILL-2  
  **Severity**: blocker  
  **Where**: `docs/sessions/05-skills.md` §Decided, `docs/architecture.md` §6, `docs/decisions.md` (risk 7)  
  **Claim**: Deriving guards from primitive effects is the right direction, but the compiler can only be correct if every primitive's effect declaration is complete and exact. The design acknowledges this is a hard requirement ("effect declarations of primitives must be exact for the compiler to derive guards") yet has no concrete mechanism to enforce it.  
  **Evidence**: Session 5 states "LLMs write wrong guards for consumed items (survey/03)" and the compiler is meant to fix this, but Session 11 says "framework code computes postconditions... never from the skill's own report" - this is enforcement on observed effects, not a check that the effect schema itself is correct. Risk 7 in `docs/architecture.md` §11 calls this out explicitly.  
  **Recommendation**: Make primitive effect declarations a machine-checkable schema (consumed, produced, placed, broken, damaged, moved, etc.), require unit tests per primitive that assert declared effects match observed deltas in the fake world, have the guard compiler reject skills where derived preconditions cannot cover declared consumes, and add a linter that fails the build if a primitive's effect set is missing or ambiguous.  
  **Confidence**: high

- **ID**: SKILL-3  
  **Severity**: blocker  
  **Where**: `docs/sessions/10-autonomy.md`, `docs/sessions/11-skill-learning.md`, `backlog.md` (§Session 10 curriculum comparison)  
  **Claim**: The user chose a free Voyager-style curriculum over a ladder spine. The design states "Praxion chooses its own goals with a free, Voyager-style curriculum" but provides no strong learning signal tied to the capability target (Ender Dragon). Goals may be unverifiable or pull focus away from the next rung.  
  **Evidence**: Backlog explicitly calls for an arm "ladder spine with code-listed options" next to the free curriculum in an equal-compute learning test because "check how often free goals end up `unverifiable`" and "log goals that keep Praxion away from the next rung". Session 11 says "the gain may be zero; that is still a result" but the first demo is described as "must be flawless" (Session 1.5) - these are in tension.  
  **Recommendation**: Treat "rung advancement" as the primary learning metric, not just "number of new skills learned". Add a verifiability filter to the curriculum (reject or decompose goals that cannot be checked within the perception policy), track "time-to-next-rung" per seed, and make the equal-compute test required before committing to the free curriculum long-term.  
  **Confidence**: high

- **ID**: SKILL-4  
  **Severity**: major  
  **Where**: `docs/sessions/11-skill-learning.md` §Minor picks (rollback), `docs/architecture.md` §4 (receipts)  
  **Claim**: Rollback is triggered only when "the active version's Wilson upper bound falls below its parent's lower bound after at least 8 uses". This is far too slow for dangerous regressions (e.g. a skill that causes fall damage, misuses a bucket, or violates rules) and 8 uses may take hours/days to accumulate.  
  **Evidence**: Session 4 requires a safe-takeover exception and that dangerous moments hold keys until the user touches them, but skill-level rollback has no equivalent fast path. Session 11 also requires red-team tests with 0 bypasses, implying catastrophic skills are a concern.  
  **Recommendation**: Add immediate rollback on critical failure classes (player death attributable to the skill, protected-block rule violation, take-over forced by danger caused by skill actions, or receipt-level verification failures). Keep the Wilson bound as the long-term retirement metric, but use a fast circuit breaker for safety-critical contexts.  
  **Confidence**: high

- **ID**: SKILL-5  
  **Severity**: major  
  **Where**: `docs/sessions/11-skill-learning.md` §Minor picks (effects), `docs/architecture.md` §5-§6  
  **Claim**: "Block changes are attributed to Praxion's actions through sequence numbers" is fragile. In a live world, packets can be delayed, other events can occur, and sequence numbers alone don't prove causality. The practice room helps, but the design doesn't specify how to handle attribution ambiguity.  
  **Evidence**: Session 4 requires "the server's word" for success and attributes via receipts, but skill admission says effects are "measured by framework code from the proxy" using sequence numbers. This is a different attribution mechanism than the action receipt ladder and needs to be reconciled.  
  **Recommendation**: Attribute effects using action receipts (link skill step -> primitive action -> receipt id) as the primary mechanism, and only fall back to world diffs for effects that have no action receipt (e.g. passive effects). In the practice room, use an isolated test server copy with no other actors. Mark attribution as ambiguous and fail admission if causality cannot be established.  
  **Confidence**: high

- **ID**: SKILL-6  
  **Severity**: major  
  **Where**: `docs/sessions/05-skills.md` §Minor picks (composition), `docs/architecture.md` §6  
  **Claim**: Composition is limited to depth <= 2 and max 30 rules per skill with "small skills of about 2 to 5 steps". This is a reasonable starting constraint, but there's no evidence it's sufficient and no plan to relax it based on measurements. Combined with "write top-down, admit bottom-up", this could prevent learning the hierarchical skills that make Voyager-like systems scale.  
  **Evidence**: Session 11 says "depth is capped at 2" and Backlog item for Session 5 says "check the format stays readable at 20 to 30 rules" - it's treated as a potential issue to measure, not a fixed requirement.  
  **Recommendation**: Don't hard-cap depth until measurements show it's necessary. Instead, gate complexity: track cyclomatic complexity, number of primitives called, and admission failure rate. If deeper skills are needed, require stricter gates (more seeds, longer probation) rather than an arbitrary depth limit.  
  **Confidence**: medium

- **ID**: SKILL-7  
  **Severity**: major  
  **Where**: `docs/architecture.md` §6 (admission), `docs/sessions/04-action-verification.md`  
  **Claim**: Skill admission verifies effects in the practice room, but doesn't explicitly require the same receipt-level verification standard as runtime actions. A skill could appear to succeed by world state changes but not satisfy "server's word" requirements, or could be admitted based on unconfirmed effects.  
  **Evidence**: Session 4 says "an action succeeds only with the server's word (level 2 for world changes, level 3 when an effect is expected). No confirmation gives `unconfirmed`..." but Session 11's gate says "effects are measured by framework code from the proxy" - it doesn't say "only count effects that reached receipt level 2/3".  
  **Recommendation**: Make the admission gate use the same receipt ladder as runtime. A practice room run only counts as success if all skill steps that change the world reached the required receipt level. Mark runs with `unconfirmed` effects as failures for admission purposes.  
  **Confidence**: high

- **ID**: SKILL-8  
  **Severity**: minor  
  **Where**: `docs/sessions/11-skill-learning.md` §Decided (approval), `docs/architecture.md` §7 (chat injection)  
  **Claim**: "A skill learned in Praxion's own play and passing the gate becomes active without asking; a skill taught through chat needs the user's approval." This is a good boundary, but "learned in its own play" is vague. Does it include skills proposed by the LLM in response to a goal it chose, or only skills it discovered by trying? Also the user needs to be notified - no mechanism specified.  
  **Evidence**: Chat is called an injection surface (Session 9), so approval makes sense. But the distinction between "own play" vs "taught through chat" needs to be unambiguous for the code.  
  **Recommendation**: Define "learned in its own play" precisely: skills generated by the brain's skill writer in response to a goal from the curriculum (or an internally generated goal) without a direct chat instruction to "write/create skill X". Skills generated in response to chat commands require approval. Add an audit log entry for auto-activated skills.  
  **Confidence**: medium


### Ideas (brainstorm)

1. **Effect schema linter as a gate** - Make primitive effect declarations first-class and lint them against observed behavior in the fake world. This prevents the guard-derivation house of cards from collapsing.
2. **Mutation testing for the admission gate** - Before accepting the gate, run the red-team: generate mutants (no-op, wrong count, side effect, protected-block write) and require 0 bypasses. Make this a precondition for enabling auto-activation.
3. **Verifiability-first curriculum** - Filter goals to those with computable completion conditions within the perception policy. Decompose unverifiable goals into verifiable subgoals or reject them with explanation. This directly addresses SKILL-3.
4. **Skill utility scoring** - Track reuse count, success rate across contexts, time-to-execute, and rollback history. Retire skills that are never reused or have poor utility even if they pass the gate.
5. **Deterministic practice fixtures** - Use pre-registered fixtures with fixed seeds for admission (not just "copy of world folder") so results are reproducible across machines and gate versions.
6. **Fast safety circuit breaker** - Implement immediate rollback for critical failures (death, rule violation) independent of Wilson bounds. This is cheap insurance and addresses SKILL-4.
7. **Receipt-linked admission** - Require every world-changing step to have a valid receipt at runtime level to count toward admission success. This unifies verification across action layer and skill layer.


### Questions for the user

1. **Verifiability vs ambition**: For the free curriculum, when Praxion proposes a goal that the code deems unverifiable under the perception policy (e.g. "find diamonds" without sufficient lighting/exploration guarantees), should it be rejected, decomposed, or allowed with a warning and not counted toward rung progression?
2. **Auto-activation scope**: Is "learned in its own play" strictly limited to skills written by the skill writer in response to internally chosen goals, or does it include skills written in response to the user's commands that were executed autonomously?
3. **Admission strictness**: For the first demo (flawless first night), should the admission gate be more strict than the long-term target (e.g. higher n, longer probation, or require human approval for all new skills initially)?
4. **Rollback SLA**: What's the acceptable time window for rolling back a dangerous skill once detected (immediate vs within N minutes)? This affects whether a fast circuit breaker is required before enabling auto-activation.
5. **Practice room fidelity**: Is the protocol test body expected to be faithful enough for admission, or must admission always run against a real test server (separate process) to catch OS-input/hardware timing issues?

FINAL_REPORT The three most critical issues are: (1) k=3 admission runs are statistically insufficient for stochastic Minecraft tasks and need seed counts with confidence intervals, (2) guard derivation depends entirely on exact primitive effect declarations with no enforcement mechanism yet, and (3) the free Voyager-style curriculum lacks a verifiability filter and a rung-progression learning signal, creating tension with the "flawless first demo" requirement. These must be resolved before enabling auto-activation of LLM-written skills.
