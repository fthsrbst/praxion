# Decisions needed before code

Research for the user, 2026-10-27. Nothing here is a decision. Sources: `docs/discussion-plan.md`,
`docs/decisions.md`, `docs/sessions/01-*.md`, `docs/sessions/02-*.md`, `backlog.md`, and the
"Challenges to recorded decisions" sections of `docs/research/survey/01..04`.

## 1. Where we stopped

- Session 1 (scope) is closed. Session 2 (body) has 2.1 presented; 2.2 to 2.7 are open.
- 2.2 (proxy) was paused on 2026-10-25 so the research survey could run. The survey is now done
  (4 topics) and the memory review is drafted; neither is walked through with the user yet.
- Recorded decisions: 16 entries in `decisions.md` (product, body direction, success measures).

## 2. Open decisions, counted

Counts are the questions listed per session in `discussion-plan.md`; real sessions will add some.

| Group | What | Open items |
|---|---|---|
| Carried | Survey challenges needing user approval: smoothness as starvation gap, Jev/hosted deadline rule (no hosted call on a path under ~1 s), hosted degraded mode, trust tiers for LLM-written code | 4 |
| Carried | License (backlog); "Praxion is active" indicator | 2 |
| Session 2 | 2.2 proxy, 2.3 hands, 2.4 hand-over, 2.5 body port, 2.6 language/runtime, 2.7 experiments | 6 sub-topics |
| Session 3 | Perception policy, staleness, screenshots, memory storage (+5 questions in `memory-world-state.md`) | 4 (+5) |
| Session 4 | Evidence level per action, failure taxonomy, verifying invisible tasks | 3 |
| Sessions 5-8 | Skill contract, reactive vs script, reflex arbiter, Jev/LLM split | 14 |
| Sessions 9-15 | Commands, autonomy, learning, memory/RAG, natural feel, testing, ops | 25 |

Total: about 58 items. Not all block code.

## 3. What actually blocks code (dependency order, principle 3)

Code is built bottom-up. The first code is the body, and it needs fewer decisions than the full plan.

**Gate A: first spike code (throwaway experiments, 2.7).** Needs only:
- 2.2 proxy shape (separate process, byte-forwarding, data reports, MCProtocolLib vs own decoder)
- 2.3 hands (Windows input method, focus rule vs `pauseOnLostFocus`)
- 2.6 language/runtime (Windows tick timing)
- 2.7 pass criteria (the survey proposes: input-to-packet latency, one-tick key hold, aim within 1 degree in 150 ms, tick-timed click, water-bucket save, decode-to-arbiter p99 under 5 ms)

Spikes answer the largest unknown: does synthetic input work at endgame timing. If it fails, the
client-mod fallback changes everything after it, so this runs before sessions 3 onward.

**Gate B: first product code (body layer, repository scaffold).** Adds:
- 2.4 hand-over/take-over, 2.5 body port
- license (before the first commit of code)
- session 3 core: perception policy and where world state lives
- session 4 core: evidence level and failure taxonomy per action

**Not blocking the body layer (decide while it is built):** sessions 5-15. Skills, arbiter, Jev/LLM
split, commands, autonomy, learning, RAG and naturalness all sit above the body and world model.
Deciding them first would break "never do the last step first".

## 4. Recommended path

1. Finish session 2: 2.2 to 2.7 (6 decisions). Batch where possible (2.2 + 2.6 together; 2.3 + 2.4 together).
2. Run the 2.7 spikes. Throwaway code, results written to `docs/sessions/02-body-connection.md`.
3. Sessions 3 and 4 in reduced form (core items above, 7 decisions).
4. License decision, then scaffold the repo with the body layer. About 14 decisions (6 + license + 7) plus spikes before product code.
5. Continue sessions 5-15 in parallel with body work, each ahead of the layer that needs it.

Main alternative: discuss all 15 sessions before any code. Safer for the design, but the largest
risks (input timing, proxy decoding) stay untested for weeks and could invalidate decisions made meanwhile.

## 5. Risks to check early

- Synthetic mouse input may not reach Minecraft's raw-input path, or may lack precision (unknown until tested on Windows, not over RDP).
- Passive decoding in offline mode needs vanilla registry data; MCProtocolLib 26.3 support merged 2026-10-14 and is untested by us.
- Hosted models: every call class needs a deadline and a fallback (survey/01 challenge 1).

## 6. Progress (2026-10-29)

All 15 discussion sessions are decided. Gate B items are done except the 2.7 experiments: the
body decisions (2.2 to 2.6), sessions 3 and 4, and the licence (Apache-2.0 core) are recorded.
Remaining before product code: the architecture draft, a review round by another model, and the
2.7 experiments. See `AGENTS.md` and `backlog.md`.
