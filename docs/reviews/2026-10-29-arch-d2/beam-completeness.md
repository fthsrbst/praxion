<!-- Reviewer: Beam Free via OpenCode. Draft-2 review, lens: beam-completeness. Raw output. -->

### Findings

1. **Minor** — `architecture.md` Appendix A (final line). **Claim:** the traceability table asserts "Gaps: none found." **Evidence:** Appendix A ends with "Gaps: none found." while §7's Keys row contains no `.env` fallback and §6.3/§8 never state the lesson trigger condition. **Fix:** replace "Gaps: none found." with a short gap list (at minimum items 2 and 3 below) or add those bullets. **Confidence:** high.

2. **Minor** — `architecture.md` §7, "Keys [D]" row. **Claim:** the `.env` credential fallback from session 15 is missing. **Evidence:** session 15 reads "with a `.env` file outside the repo as a fallback"; draft §7 reads "in the OS credential store, held only by the brain, never logged". **Fix:** append "with a `.env` file outside the repo as a fallback." to that row. **Confidence:** high.

3. **Minor** — `architecture.md` §6.3 and §8. **Claim:** the lesson trigger condition is unspecified. **Evidence:** session 12 says "Lessons come when a trigger fires"; draft §6.3 gives only the acceptance rule ("proposed by the LLM, accepted by code after 3 proxy-verified successful traces") and §8 says lessons "wait for a measured trigger" (storage). **Fix:** state the trigger condition or mark it `[O]`. **Confidence:** medium.

4. **Minor** — `architecture.md` §5.1 Perception. **Claim:** the `Known(v)/Unknown(reason)` query-answer type named in R1-B7 is not defined. **Evidence:** synthesis R1-B7 asks for "a typed `Known(v) / Unknown(reason)` in every query answer"; draft §5.1 says only "`unknown` is a value, not 'absent' [D]. Out of sight, a query answers `unknown`." **Fix:** define the perception query result as `Known(v) | Unknown(reason)`. **Confidence:** high.

5. **Minor** — `architecture.md` §9, "Ideas [P]". **Claim:** ideas are not tagged with cost/phase and phase-2 work is not moved to backlog. **Evidence:** critic point 11 asks for S/M/L and first-serving-demo tags and backlog placement for shadow mode and the death-to-fixture factory; draft §9 lists "a stock-client capture corpus … death-to-fixture factory; a replay harness …; a planner-only control arm" with no tags. **Fix:** tag each idea with cost and first-serving demo; move shadow mode and the death-to-fixture factory to `backlog.md`. **Confidence:** medium.

6. **Minor** — `architecture.md` §1 / §9. **Claim:** naturalness metrics are not enumerated. **Evidence:** synthesis minor (C37+C45) records "Session 13 already defines naturalness metrics (KS / energy distance, AUC ≤ 0.6, paired forced choice)"; draft §1 says only "naturalness (movement statistics and a blind test)" and §9 lists no metric. **Fix:** add "KS/energy distance, AUC ≤ 0.6, paired forced choice" to §1 or §9. **Confidence:** high.

### Verdict

Draft 2 has absorbed every blocker and major from review round 1: the eight blockers (SDL3 hands, pinned version baseline, forwarder process boundary, timing experiment gap, command gesture, brain-independent safety, body-port/actuator/`unknown` contracts, dependency-ordered first slice) and the twelve majors (receipts, interpreter semantics, perception mechanism, single writer for state, take-over sensor, dedicated-server constraints, primitive contract, admission evidence, cost brake, planner contract, safety enforcement, traceability) are all present, and the dropped recommendations (auto-resume after take-over, re-scoping away from the dragon, dropping RAG/Jev/free curriculum, fixed N≥30, second Windows box, cancellation on the actuator frame, probation on the real body, ingest-time line-of-sight filter) are correctly absent. The remaining gaps are the six minor items above, none of which touches a contract the 2.7 experiments depend on. Draft 2 is ready to drive the 2.7 experiments, round 0 (desk) first, provided the inaccurate "Gaps: none found." claim and the missing `.env` fallback are corrected in a quick pass before the first experiment runs.

FINAL_REPORT 0 blockers, 0 majors, 6 minors.
