<!-- Reviewer: Iceman Contributor Free via OpenCode opencode v2.1.2. Lens: iceman-latency. Raw, unverified output. -->

Let us start the review.

### Findings

- **ID**: TIME-COST-001  
  **Severity**: blocker  
  **Where**: `docs/architecture.md` §3, `docs/sessions/08-jev-llm-brain.md`, `docs/sessions/14-testing.md`  
  **Claim**: "Nothing with a deadline under ~1 s may depend on Jev or any hosted call" is stated, but the architecture still leaves Jev's latency envelope and cancellation/billing behavior unmeasured and unguarded by hard enforcement.  
  **Evidence**: Jev vendor claims 70-500ms with 3P up to ~524ms (docs/research/decision-models.md:21-22); session 08 requires measuring p50/p95/p99 and whether cancelling stops billing (08:61-62) but these are in backlog with no pass/fail gates enforced in §3. Also chaos test (08:55) exists but not wired into architecture's "every call class has a deadline and fallback".  
  **Recommendation**: Treat Jev as "admissible only if measured p99 < deadline - margin" with a per-decision deadline computed at runtime; enforce "hosted calls forbidden on <1s paths" in code (compile-time/lint) not just policy; require billing-cancel test in E2.7/8 measurements before enabling Jev anywhere.  
  **Confidence**: high

- **ID**: TIME-COST-002  
  **Severity**: blocker  
  **Where**: `docs/architecture.md` §5 ("brain loop"), `docs/sessions/08-jev-llm-brain.md` §"No filler that changes the world", `docs/research/survey/02-fast-slow-architectures.md` §4  
  **Claim**: "LLM is woken by events, not a clock" with ~60s heartbeat, but the design relies on "runway > p95 latency" without guaranteeing it for Minecraft's bursty goals; "starvation gap" measure exists but there's no hard cap on dead-time before safe fallback.  
  **Evidence**: Survey 02 shows TickingCollab real-time success drops to 0.05 (02:90), AgileThinker needs parallel tiers (02:87), runway rule stated but not quantified. Session 08 says "after a grace period of 3-5 s a safe fallback goal runs" (08:53) but architecture §5 doesn't make this a non-negotiable invariant with telemetry.  
  **Recommendation**: Make "safe-stop/fallback" mandatory on any admitted program with `remaining_ms < p95(LLM)+margin` and enforce starvation-gap SLO (e.g. p99 < 1s) in tests; log and fail builds if fallback path not exercised in chaos tests.  
  **Confidence**: high

- **ID**: TIME-COST-003  
  **Severity**: major  
  **Where**: `docs/architecture.md` §7 ("Hosted dependence"), `docs/sessions/07-reflexes-arbiter.md`, `docs/sessions/04-action-verification.md`  
  **Claim**: Reflexes/take-over/ruleset must never use network - correct in principle - but "take-over exception holds keys in dangerous moments" creates coupling: the input/take-over path must know cancellation classes and world state without brain, which is feasible if state lives in body, but the architecture doesn't explicitly budget for this combined path or test it under brain-dead conditions.  
  **Evidence**: Session 04 adds exception to "release all keys" (04:35-37), session 07 says reflexes pure functions in Rust body (07:29). Brain-dead behavior specified (02:32, 02:226) but no explicit latency budget for "brain silent -> safe stop" in architecture table.  
  **Recommendation**: Add explicit budget for brain-silent transition (e.g. <100ms to safe state) and test in chaos (cut providers mid-action). Ensure cancellation-class evaluation is pure body code with no network calls.  
  **Confidence**: high

- **ID**: TIME-COST-004  
  **Severity**: major  
  **Where**: `docs/architecture.md` §2, `docs/sessions/02-body-connection.md` §2.7  
  **Claim**: Extra hop (forwarder -> body) is acceptable if E1-E3 pass (<1ms p99 forwarder, <5ms decode-to-state), but "whether forwarder is its own process or thread" is marked [O] with no decision gate - process isolation helps crash-safety but adds IPC latency; thread helps latency but may share failure modes.  
  **Evidence**: 02:49-51 says experiments must show extra hop keeps decoding within 5ms budget with isolated thread as fallback. Architecture diagram shows forwarder as separate component but boundary unspecified.  
  **Recommendation**: Decide boundary before implementing body port; require measured p99 for both forms in E1-E3 and pick the one that meets budget with margin (e.g. <2.5ms) leaving headroom.  
  **Confidence**: high

- **ID**: TIME-COST-005  
  **Severity**: major  
  **Where**: `docs/architecture.md` §6, `docs/sessions/05-skills.md`, `docs/sessions/11-skill-learning.md`  
  **Claim**: Skills are TR rule lists interpreted by body (good for avoiding brain freezes), but LLM skill admission relies on practice room + gates which are slow/costly and "learning test at equal compute" is planned - risk of expensive self-improvement loop or gates that block all learning. Also effect declarations must be exact (backlog risk).  
  **Evidence**: 05:23-36 says body interprets TR (no brain freeze), 11:15-27 requires staged admission (practice room k=3, probation), 11:49-51 learning test at equal compute. Survey 04 shows unverified self-written skills hurt (-1.3pp) and verified help; gates are necessary but expensive.  
  **Recommendation**: Start with curated core only for first demo (as advised), make gates fast-fail on static checks, run expensive stages in nightly batch, and track "skills admitted per $ spent" as a metric; enforce that effect declarations are generated from version pack where possible.  
  **Confidence**: high

- **ID**: TIME-COST-006  
  **Severity**: major  
  **Where**: `docs/architecture.md` §8, `docs/sessions/12-memory-rag.md`  
  **Claim**: Wiki RAG added for LLM knowledge (user chose against recommendation) despite CC BY-NC-SA license conflict with open-core and it only serves LLM (not Jev/reflexes) - cost/latency impact unclear, and "never distributed" is a constraint but retrieval adds tokens/calls.  
  **Evidence**: 12:32-39 says wiki RAG chosen, license conflict open, serves LLM only. 12:46-51 says Qwen3-Embedding local free but still adds retrieval time/tokens.  
  **Recommendation**: Treat RAG as optional and off critical path; require retrieval test showing it helps on real questions before enabling; have a fallback to game data tables only; resolve license before any distribution.  
  **Confidence**: high

- **ID**: TIME-COST-007  
  **Severity**: major  
  **Where**: `docs/architecture.md` §3, `docs/sessions/08-jev-llm-brain.md`, `docs/research/decision-models.md`  
  **Claim**: Jev is "wired in by measurement" but with 5-80 options and look-alikes calibration unknown; also "first-option bias" and counting weaknesses noted. If enabled without calibration it could be wrong and cause more LLM calls (cost) or bad decisions.  
  **Evidence**: 08:19-23 says measure before relying, 08:45-48 gates on calibrated probability with hysteresis. Decision models warn about weaknesses and calibration needs. Survey 04 says Jev never measured on skill selection scale.  
  **Recommendation**: Don't enable Jev in first demo; require calibration curves and look-alike audit (survey 04 idea 8) with pass thresholds before enabling; keep rules/LLM as fallbacks.  
  **Confidence**: high

- **ID**: TIME-COST-008  
  **Severity**: minor  
  **Where**: `docs/architecture.md` §5, `docs/sessions/02-body-connection.md` §2.5  
  **Claim**: Brain may run on another computer but default same PC - network adds latency/cost but also failure modes; "remote needs explicit key" noted. Not directly blocking but increases complexity for cost/latency guarantees.  
  **Evidence**: 02:28-31, 15:13-16 say remote possible but default same PC.  
  **Recommendation**: Defer remote brain to post-demo; enforce loopback-only in default config.  
  **Confidence**: medium

### Ideas (brainstorm)

1. **Latency budget as a contract**: Make per-call-class budgets (reflex <50ms, arbiter <1s, Jev <p99_margin, LLM <runway) machine-checkable in tests; fail CI if violated.
2. **"Degraded mode" first-class**: Explicit mode when providers down - body runs on curated skills + reflexes only, no new LLM skill writing, UI shows degraded state. Already implied but should be a named state machine.
3. **Cost brake that actually brakes**: Track $/hour per class with soft warn and hard stop (pause new non-critical LLM calls) rather than "never cut automatically" - "warn, never cut" conflicts with avoiding runaway cost; at least add hard stop for skill writing/curriculum.
4. **Jev as optional optimizer**: Treat Jev as A/B measured feature; default off until calibration passes; log agreement to build dataset for future training.
5. **Effect schemas from data**: Auto-generate skill effect schemas from Mojang data reports to reduce admission gate errors and speed up verification.
6. **Fixture caching**: For practice room, cache world fixtures by hash to cut admission time/cost.
7. **Starvation telemetry**: Add `starvation_gap_ms` histogram with alerts; make it a first-class metric in run reports.

### Questions for the user

1. **Jev enablement**: Should Jev be disabled for the first demo regardless of measurements, or enabled only if it passes strict calibration on day-one decision points?
2. **Cost brake policy**: Do you want "warn only" or a hard stop (e.g. stop non-critical LLM calls above threshold) to prevent runaway spend?
3. **Brain location**: Confirm default is same Windows PC only (no remote brain for first demo)?
4. **Wiki RAG**: Given CC BY-NC-SA license conflict with open-core, do you want to drop wiki RAG and rely on game data + LLM knowledge, or keep it local-only with explicit non-commercial constraints?
5. **Skill learning scope**: For first demo, should LLM skill writing be disabled entirely (curated core only) or allowed only in practice room with no auto-admission?

FINAL_REPORT The design is strong on safety boundaries (reflexes off-network, receipts, take-over), but the biggest risks are timing/cost guarantees: Jev latency/billing unmeasured with no hard enforcement, runway-based brain loop lacks a mandatory safe-fallback SLO, and the extra hop/process boundary plus expensive skill gates could break "fast and cheap enough to survive first night" without making degraded mode and measurement gates blocking. Treat measurement gates as hard blockers before enabling hosted decision tiers.

