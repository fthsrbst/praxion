# Session 8: Jev and the LLM brain

Decided 2026-10-29. Inputs: `docs/research/decision-models.md`, survey/02 (async tiers, event
triggers, stamped directives, latency sweeps), survey/01 ideas 4 to 6, survey/04 (Jev option count).

## The three tiers
| Tier | Time scale | Work |
|---|---|---|
| Code (Rust body) | milliseconds | reflexes, running skills, guards, take-over |
| Jev | about 0.15 to 0.5 s | bounded choices the code lists (chat to skill, keep or wake the LLM, interim pick, mixed danger judgement) |
| LLM | seconds | planning, chat, writing rule-list skills |

Rules from the research: no choice with a deadline under about 1 s may depend on Jev or on any
hosted call (Jev measured 130 to 524 ms); Jev decides only when the deadline exceeds its p99 plus
a margin, measured from the user's PC. Honest status: Jev's value for Praxion is unproven
(System1Doom: hand-written controllers often beat the models), so it is measured before it is relied on.

## Decided (user, 2026-10-29)
- **Jev is the middle tier and is wired in by measurement.** It works only on options the code
  lists, with a deadline above 1 s. At each decision point it is compared with rules and with the
  LLM; where it is not better it is not used. The backend stays swappable (Jev, open decision
  models). Reflexes never touch it. Rejected for now: no Jev in the first demo, and Jev polling
  every second.
- **The LLM is called by events, not by a clock.** A closed list of causes (program exhausted,
  monitor fired, skill failed, chat, new threat, take-over ended, ruleset refusal) plus a
  heartbeat of about 60 s that first asks "did anything change?"; a rate limit per cause. Rejected:
  every N seconds, every step.
- **Cost brake: warn, never cut.** Cost per hour is tracked and the user is warned above a
  threshold the user sets. Every call class has a deadline and a fallback, so a network outage or
  empty credits makes the body stop safely rather than break.
- **Smoothness is measured as a starvation gap** (amends measure 3 of decision 1.6): time with an
  active goal, no declared wait and no packets seen by the proxy. Ordinary pauses do not count.
  The raw "longest gap without input" is kept as a diagnostic only.
- **Reinforcement learning is a noted idea, not in the first demo.** Primitives stay swappable
  (state in, controls out at 20 Hz), so a learned policy could replace a hand-written one later.
  Candidates: combat micro, bridging, imitation of the user's own motor style. Recorded in `notes.md`.

## Minor picks by Claude (user may overrule)
- **Roles:** planner and skill writer (large model), chat (fast model, may be the planner at
  first), critic (a different model from the writer, principle 6). Models are chosen by
  measurement: p50/p95/p99 latency from the user's PC, whether cancelling stops billing, and
  whether the API streams structured output.
- **State for Jev:** the code computes every number and passes short text with buckets
  ("health: low", "2 hostile within 8 blocks"); Jev's weakness with counting is avoided.
- **When Jev is confident and wrong:** gate on calibrated probability, not the top answer; low
  confidence falls back to rules or the LLM; a new answer replaces a committed one only if it
  beats it by a margin (hysteresis); every decision and its outcome is logged; Jev never decides
  anything the user's ruleset protects, and the ruleset is enforced by code.
- **Stamped directives:** every LLM or Jev directive carries `{id, generation, source tick, model,
  latency, predicted}`; a result is adopted only if the generation is unchanged and the guards
  hold on actual state. The next subgoal is planned early, when the remaining time is below
  the model's p95 latency plus a margin.
- **No filler that changes the world:** while waiting, only camera movement or nothing; after a
  grace period of 3 to 5 s a safe fallback goal runs and the starvation is logged as a fault.
- **Chaos test:** cut the providers mid-night and inject 5, 15 and 30 s of LLM latency; the
  longest starvation stays under 1 s and preemption under 50 ms.

## Left open, with owner
- Reflex coverage and arbitration tiers: session 7. Command handling: session 9. Skill admission:
  session 11. Goal generation: session 10.
- Measurements (backlog): p50/p95/p99 of Jev and candidate LLMs from the user's PC; whether Jev
  stays calibrated with 5 to 80 options and look-alike skills; whether APIs stream structured JSON.
