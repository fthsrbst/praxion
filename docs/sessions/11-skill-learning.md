# Session 11: Skill learning and LLM-written code

Decided 2026-10-29. Inputs: survey/04 (admission by re-execution, repair-versus-regression gate,
per-context attribution, skill networks, lineage), survey/03 idea 7, systems-review
(agent-swarm candidate and approval pipeline, Craftonomous reliability), sessions 4 and 5.

## What the research says
- Writing a skill is cheap; deciding which to keep is the work. Unverified self-written text
  skills lowered scores slightly (SkillsBench, minus 1.3 points); verified programs helped.
- Choosing among skills degrades as the library grows (over 90% right from 20 skills, about 20%
  from 200; look-alikes cost 17 to 63%). Jev has not been measured on this.
- Most learned skills are used once. "Praxion learns" needs a comparison at equal compute.

## Decided (user, 2026-10-29)
- **A new skill is tried in a practice room first.** Admission has stages: static checks (parse,
  types, banned constructs, contract, called skills active); the skill runs in a copy of the world
  on a separate test server, k=3 times; then a probation period watched on the test server only;
  then active. Rejected: trying directly in the user's world and static checks alone.
- **Approval:** a skill that Praxion learns in its own play and that passes the gate becomes
  active without asking (the user is notified; it can be rolled back). A skill taught through
  chat needs the user's approval, because chat is an injection surface. The ruleset is enforced
  by code either way.
- **Learning by watching the user: record now, learn later.** The user's own play is recorded
  locally (moves and world state) but no skill is extracted from it in the first version. The data
  later feeds the movement-style model (session 13) and perhaps skill induction.

## Minor picks by Claude (user may overrule)
- **Skill states:** candidate, probation, active, retired; quarantined and deprecated are flags.
  Any edit returns a skill to candidate. The hand-written core passes the same gate first.
- **The gate compares with a baseline:** k=3 runs on fixtures against runs without the skill
  (McNemar test), so a skill must repair more than it breaks. An LLM judge never decides.
- **Effects are measured by framework code from the proxy** (typed delta of inventory, blocks,
  position, entities, health), checked against the skill's declared schema, never from the skill's
  own report. Block changes are attributed to Praxion's actions through sequence numbers.
- **Trust zones:** the skill writer is a separate process that can write only to
  `skills/candidates/`; only framework code computes postconditions; evidence logs are append-only.
  A red-team LLM tries to get 20 broken skills admitted, including through chat injection; the
  target is 0 bypasses.
- **Every real success becomes a regression fixture** (copy of the touched chunks or world folder,
  the player's data, the goal and the effect signature). Fixtures are restored only on the test
  server, never in the user's world.
- **Versions and rollback:** skill versions live in git with one active pointer per skill
  family. Roll back when the active version's Wilson upper bound falls below its parent's lower
  bound after at least 8 uses. Retire only when the upper bound is low in every context with at
  least 8 events and the failures are typed "skill broken" (session 4), otherwise try a split.
- **Write top-down, admit bottom-up:** a parent is written against children that do not exist yet;
  each child is 2 to 5 steps with its own fixture and is gated first; depth is capped at 2.
- **A learning test at equal compute:** two arms at equal tokens per hour, learning on versus a
  frozen hand-written core that spends its budget on retries, at least 20 paired seeds on rungs
  1 to 3, analysis written before the runs. The gain may be zero; that is still a result. Kept
  apart from the claim that the first demo is flawless. Owner: sessions 14 and 15.
- **Recording of the user's play:** stored locally, never sent to a hosted model, only while the
  game window is focused and Praxion is not playing; typed chat text is not stored unless it is a
  command; the user can delete it.
- **Jev selection audit:** measure how many options remain after code filtering and Jev's accuracy
  at 5 to 80 options with look-alikes; add a category level only if the 95th percentile exceeds
  the measured limit.

## Left open, with owner
- Lessons as text (a second store with counters, promotion to code after agreement): session 12.
- Gate thresholds, fixture restore time, k, probation length: measured during implementation.
- Whether a protocol test body is faithful enough for the OS-input body: experiments (2.7) and session 14.
- Process isolation on Windows for the skill writer: session 15.
