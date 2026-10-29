<!-- Reviewer: Claude Sonnet 5.5 (Claude Code workflow subagent). Lens: sonnet-models. Raw, unverified output. -->

### Findings

**HOST-1 — major — `docs/architecture.md` §3, `docs/sessions/08-jev-llm-brain.md` "The three tiers"**
- **Claim:** The architecture says the LLM tier works at "seconds". For a reasoning model this is wrong by 1-2 orders of magnitude, and the plan-early rule is unworkable at those latencies.
- **Evidence:** Artificial Analysis lists time to first answer token for Claude Sonnet 5 at 10.02 s (high effort), 29.89 s (xhigh) and 102.52 s (max). End-to-end is 18.4 s, 37.6 s and 108 s. DeepSeek V4.1 Flash (max) is 11.9 s to first answer token and 14.65 s end-to-end for 500 tokens. Gemini 3.7 Flash (medium) is 4.47 s. (https://artificialanalysis.ai/models/comparisons/gemini-3-7-flash-vs-claude-sonnet-5-high, https://artificialanalysis.ai/zh/models/comparisons/deepseek-v4-1-flash-vs-claude-sonnet-5.) Session 8 plans the next subgoal "when the remaining time is below the model's p95 latency plus a margin". With a p95 of 30-100 s, that trigger fires almost always.
- **Recommendation:**
  - Pin per-role latency budgets: non-reasoning or low-effort for chat and heartbeat, bounded thinking for the planner, and the large model only for skill writing, where latency is hidden.
  - Make `thinking budget` and `effort` explicit parts of the "role" definition.
  - Cap the plan-early lead time (e.g. 60 s), because a p95 of 100 s would otherwise trigger planning every time.
- **Confidence:** high

**HOST-2 — major — `docs/sessions/08-jev-llm-brain.md` "Cost brake", `docs/decisions.md` L65, `docs/sessions/15-infrastructure.md` L28**
- **Claim:** "warn, never cut" has no ceiling on a runaway autonomous loop, and it contradicts the "empty credits make the body stop safely" line.
- **Evidence:**
  - Rate limits are only "per cause". A loop of skill fails, the "skill failed" event wakes the LLM, the LLM rewrites the skill, the skill fails again can run indefinitely.
  - Overnight at Fable 5.1 prices ($10 in / $50 out per MTok, https://platform.claude.com/docs/en/about-claude/pricing) my estimate below is about $26/h typical and about $125/h in a thrash scenario. A warning no one sees is worthless.
  - Empty credits already cut service, so the user's decision is effectively "cut by the provider, mid-night, uncontrolled".
  - Session 15 already gives test runs a hard budget stop, so the policy is inconsistent inside the docs.
- **Recommendation:**
  - Keep "warn" as the first stage, and add a circuit breaker that limits rate, not money: a global cap on LLM calls per minute, and a consecutive-failure breaker that degrades to a cheaper model, then to a code-only mode.
  - Add a very high hard stop the user sets once, with a loud announcement.
  - Also set a provider-side spend limit (unverified for each provider).
  - Ask the user to re-decide this one point with the numbers in the cost table.
- **Confidence:** high

**HOST-3 — major — `docs/sessions/08-jev-llm-brain.md` "No filler" and "Chaos test", `docs/architecture.md` §3 rules**
- **Claim:** Outage behaviour is internally contradictory and has no defined safe state.
- **Evidence:**
  - Three statements conflict. "A silent brain makes the body stop safely." "After 3 to 5 s a safe fallback goal runs and starvation is logged as a fault." The chaos test requires "longest starvation stays under 1 s" with 5, 15 and 30 s LLM latency.
  - Starvation is defined as "no packets seen by the proxy". The client sends position packets at 20 Hz whenever it is connected, so on a literal reading the metric can never trigger. This is unclear and needs a definition.
  - Stopping in the open at night in survival gets you killed. "Stop safely" is not defined.
- **Recommendation:**
  - Define the fallback as a precomputed local skill in the body: a night shelter, or a "retreat and wait" built from the interpreter. It needs no network and is verified by tests.
  - Define starvation as an observable that excludes keepalive and movement-neutral packets, for example no state-changing intent for more than T seconds.
  - State a per-call-class deadline table (command parse, chat reply, heartbeat, plan, skill write) with a fallback for each. The architecture claims this exists, but §3 only states the rule.
- **Confidence:** high

**HOST-4 — major — `docs/research/decision-models.md` L18-L28, `docs/architecture.md` §11 row 5, `docs/decisions.md` L202**
- **Claim:** Jev's cost is irrelevant, so only latency and calibration can justify it. Its vendor risk is not recorded in any decision.
- **Evidence:**
  - Price is $0.042 per 1M input tokens, output free (https://docs.typesafe.ai/models). A Jev call of about 1.5k tokens costs roughly $0.00006. The teknium dragon run cost $0.97 in total, of which Jev was $0.01 for 119 decisions. The planner was about 99% of the spend (https://github.com/teknium1/hermes-and-jev-play-minecraft).
  - The 1.14 post calls 1.14 a "preview release" with a 30-day deprecation notice, and 1.13 gets 60 days. It promises a deprecation policy that is not yet published (https://typesafe.ai/blog/jev-1-14).
  - The docs state no SLA, no uptime and no latency figures (https://docs.typesafe.ai/models).
  - eesel reports that TypeSafe "can't prove" the price is not subsidised and that access was by waitlist. This is a secondary source with a commercial interest, so I mark it unverified (https://www.eesel.ai/es/blog/jev-ultrarrapido).
  - OpenRouter shows a single provider (the model has no failover) and 99.99% availability over 3 days. That is a 3-day window.
- **Recommendation:**
  - Record in the decisions that Jev's role depends on three things: the latency gap versus a cheap LLM at the same decision, calibration value, and one vendor with no SLA.
  - Make the `/v1/systemone` abstraction a first-class port with a built-in "LLM structured-choice" implementation, so removing Jev costs nothing.
  - Pin `jev-1.13.0` or `jev-1.14.0` (aliases move; `jev-latest` points at 1.14.0).
- **Confidence:** high

**HOST-5 — major — `docs/sessions/08-jev-llm-brain.md` "Decided", `docs/research/decision-models.md` "Design implications"**
- **Claim:** The case for Jev as a middle tier versus rules plus one cheap LLM is weaker than the docs imply, and the docs do not name a falsifiable bar.
- **Evidence:**
  - The documented Jev latency is 130-524 ms, plus network from Turkey to a US provider. The region is unverified.
  - A cheap non-reasoning LLM at 0.5-1.5 s still meets every deadline Jev may take, because Jev only handles deadlines above 1 s. The gap between 0.2 s and 1 s matters only for the 1-3 s class, and the docs list few such decisions.
  - The research doc itself says that in System1Doom "the hand-written controller dominated" and a random policy matched the models on some scenarios.
  - Jev's real differentiators are calibrated probabilities, many questions in one pass, and no free-text parsing. These are real, but a cheap LLM with constrained or structured output plus code-computed thresholds gets the same for command parsing.
  - Jev is weak exactly where Praxion's states are rich: counting, dates, large state, first-option bias, state text that steers answers (https://docs.typesafe.ai/model-jaggedness/jev-1.13). Command text reaches Jev as state.
- **Recommendation:**
  - Make "Jev proved better than a cheap LLM on logged decisions" the entry bar. Build the system with the cheap LLM as the default tier. Add Jev as a measured plug-in, as an experiment, not as a tier in the architecture diagram.
  - Define the measurement now: replay the same logged decision set through rules, a cheap LLM and Jev, and compare accuracy, p95 latency and a calibration error such as ECE at an agreed sample size.
- **Confidence:** medium (the cheap-LLM latency for non-reasoning calls is my expectation, not measured here; I found no sourced figure)

**HOST-6 — major — `docs/architecture.md` §5 "The brain loop"**
- **Claim:** The heartbeat "first asks 'did anything change?'" without saying who answers. If a model answers, it is a call class with its own cost and failure mode. If code answers, it should be stated.
- **Evidence:**
  - A 10-minute night has about 10 heartbeats.
  - Teknium's planner ran 35 calls in about 8 minutes, so the event rate is dominated by events, not the heartbeat.
- **Recommendation:** The change check must be code: a hash or diff over the briefing facts. A heartbeat that costs a model call only when the diff is nonzero is cheap and has no latency path. Write it into the decision as a rule.
- **Confidence:** medium

**HOST-7 — major — `docs/architecture.md` §4 (Jev and LLM clients), §7 "Hosted dependence", §11**
- **Claim:** There is one hosted dependence class but no multi-provider design. A provider-wide outage or a 429 or 5xx storm is a single point of failure for all brain functions.
- **Evidence:**
  - Jev sits behind one provider (OpenRouter shows one). Rate limit 429s are per-organisation (https://docs.typesafe.ai/models). The SDK retries 429s by default, so retry can add latency inside your deadline.
  - No document describes provider failover, retry budgets inside deadlines, or an offline "reflex plus precomputed skills only" mode with its limits.
- **Recommendation:**
  - Add per-role provider lists (primary, secondary), a retry budget that is part of the deadline, and a circuit breaker per provider.
  - Turn off SDK auto-retry or bound it. Log the provider, model id and request id for every call.
  - Define the offline mode explicitly: which rungs it can still make progress on.
- **Confidence:** medium

**HOST-8 — minor — `docs/sessions/08-jev-llm-brain.md` "Roles"**
- **Claim:** The roles table names no concrete models. It also lacks the price-sensitive dimension: whether cancelling stops billing, and prompt-cache behaviour.
- **Evidence:** Anthropic's cached input is 5x to 20x cheaper than uncached (Sonnet 5.5: $2 in, $0.10 cache read; Opus 5.5: $4 / $0.20; Haiku 5.5: $0.10 / $0.01; Fable 5.1: $10 / $0.25). Cache TTL is 5 minutes or 1 hour. A stable system prompt plus primitive catalog should be placed as a cacheable prefix. A briefing that changes at each call must go after it. A 60 s heartbeat keeps a 5-minute cache alive. A batch API at 50% off applies only to async work such as skill-writing off-line candidates.
- **Recommendation:** Specify the prompt layout (cacheable prefix, then volatile briefing). Route non-urgent skill writing and critic runs to the batch API where deadlines allow.
- **Confidence:** high

**HOST-9 — minor — `docs/research/decision-models.md` L18-L28**
- **Claim:** The research cites conflicting context limits and presents an uncertain "[U]" row as design input.
- **Evidence:** Jev 1.13 context is 64k per request in the TypeSafe docs, 32k on the OpenRouter listing, and 32k in the eesel report citing Cloudflare. Long context is off by default and needs the `TypeSafe-Danger-Zone-Long-Context: true` header. 1.14 offers 262k. The doc notes 80 req/s and 100k tokens/s; the docs say limits are per organisation and "current limits are in the developer console", so these are not public numbers. I could not verify them.
- **Recommendation:** Mark those numbers as account-specific unverified values. Do not size designs on them.
- **Confidence:** medium

**HOST-10 — minor — `docs/sessions/10-autonomy.md` "free curriculum", `docs/architecture.md` §6**
- **Claim:** The free LLM curriculum plus an LLM-written skill loop makes call volume depend on skill failure rate, which the cost estimate cannot bound. This ties cost directly to quality measures.
- **Evidence:** Session 11 says the practice room runs three runs against a baseline for every skill. Each is a hosted-model event if the LLM critiques or repairs. The 15-test budgets stop runs but the night policy does not.
- **Recommendation:** Add "LLM calls per rung" and "dollars per rung" as logged success measures next to the ladder, and a limit on repair attempts per goal (e.g. 3) before the goal is marked failed.
- **Confidence:** medium

**HOST-11 — minor — `docs/sessions/08-jev-llm-brain.md` "State for Jev"**
- **Claim:** Latency should be measured as an end-to-end distribution including TLS and queueing, not only vendor figures. The docs only plan a one-time measurement.
- **Evidence:** Vendor claims 70-500 ms and third parties 130-320 ms. Cloudflare measured 524 ms. OpenRouter shows 0.15 s p50 best-provider latency (search snippet). The one external quote of "18x faster p95" is a single use case (Rauch, via eesel, unverified).
- **Recommendation:** Build a continuous latency probe from the user's PC into the client, so deadline margins come from live p99 over a rolling window, not a one-time benchmark. Use the live p99 as the gate in the rule "deadline exceeds p99 plus margin".
- **Confidence:** medium

### Cost estimate for one 10-minute night

**Assumptions** (mine, not from the docs; Teknium's run is the only anchor, with 119-131 Jev decisions and 35-52 planner calls in 8-12 minutes):
- Jev: about 120 calls × 1.5k tokens = 180k tokens → about $0.008. Cost is irrelevant.
- Heartbeats: 10, code-only diff (see HOST-6).
- LLM calls by the event policy: 20 (calm), 40 (typical), 90 (thrash with skill-writing).
- Input 10k tokens per call, 70% served from cache. Output 800 / 1.5k / 4k tokens for calm / typical / thrash.

| Model (price in / out per MTok) | Calm | Typical | Thrash | Typical per hour |
|---|---|---|---|---|
| Haiku 5.5 ($0.10 / $0.50) | about $0.01 | about $0.07 | about $0.25 | about $0.4 |
| Sonnet 5.5 ($2 / $10) | about $0.29 | about $0.87 | about $4.2 | about $5 |
| Opus 5.5 ($4 / $20) | about $0.58 | about $1.74 | about $8.4 | about $10 |
| Fable 5.1 ($10 / $50) | about $1.3 | about $4.3 | about $21 | about $26 |

Hourly thrash on Fable 5.1 is about $125/h. Per-hour figures assume the typical event rate holds for a whole hour, which a real run will not.

### Ideas

- Run the Jev vs cheap-LLM vs rules comparison as a replay harness on logged decisions. The event log already exists, so it costs nothing.
- Make the heartbeat free: a code diff over briefing facts, with the model woken only on change.
- Add a "degrade ladder" in code, in order: big model, cheap model, rules only, shelter.
- Log dollars and call counts in each ladder rung record, so cost is a research output, not a surprise.
- Split reading from writing: use batch pricing (50% off) for skill-writing candidates, since they are not urgent.
- Test the chaos scenario with real provider errors (429, 5xx, slow stream) from a fake proxy, in addition to injected latency.
- Prefix-cache the primitive catalog and system prompt once per session, so the typical call is mostly cache reads.

### Questions for the user

1. Do you accept replacing "warn, never cut" with warn plus a rate circuit breaker, plus one high hard stop you choose? If not, what is the highest amount per hour you could tolerate unattended?
2. Which provider, and which model per role, is your first choice? The cost spread is about 20x between Haiku 5.5 and Fable 5.1 (typical: $0.4/h vs $26/h).
3. Is the Jev experiment worth a spike before architecture draft 2, or should the architecture be written with a cheap LLM as the default and Jev only an optional later plug-in?
4. What is your tolerance for chat reply latency? A reasoning model would take 10-30 s; a non-reasoning one takes 1-3 s (unverified).
5. May the fallback during outage be a precomputed local shelter or retreat skill, even though that means the agent will not progress on the ladder during that time?

### Sources

- https://docs.typesafe.ai/llms.txt
- https://docs.typesafe.ai/models
- https://docs.typesafe.ai/model-jaggedness/jev-1.13
- https://typesafe.ai/blog/jev-1-14
- https://www.eesel.ai/es/blog/jev-ultrarrapido
- https://github.com/teknium1/hermes-and-jev-play-minecraft
- https://platform.claude.com/docs/en/about-claude/pricing
- https://artificialanalysis.ai/zh/models/comparisons/deepseek-v4-1-flash-vs-claude-sonnet-5
- https://artificialanalysis.ai/models/comparisons/gemini-3-7-flash-vs-claude-sonnet-5-high
- Search results only, not fetched: https://openrouter.ai/models/typesafe/jev-1.14 (the page returned 404 on direct fetch; the uptime and p50 figures are from the search snippet and are unverified), https://benchlm.ai/stats/llm-pricing, https://tokenhot.ai/blog/llm-api-pricing-comparison-2026

I read `docs/architecture.md`, `docs/decisions.md`, `docs/sessions/08-jev-llm-brain.md`, `docs/sessions/10-autonomy.md` and `docs/research/decision-models.md`. I only grepped `docs/sessions/02`, `09`, `11` and `15`, and did not open `docs/reviews/` or `docs/research/survey/`. I made no file changes.