<!-- Reviewer: Iceman Contributor Free via OpenCode opencode v2.1.2. Lens: iceman-testing. Raw, unverified output. -->

I'll start by reading more details on the test plan from sessions 4, 7, 11 and the full 2.7 experiments again, plus any research on testing.


### Findings

#### TESTING-1
- **ID**: TESTING-1
- **Severity**: blocker
- **Where**: `docs/architecture.md` §9, `docs/sessions/14-testing.md`, `docs/sessions/02-body-connection.md` §2.7
- **Claim**: The three-level pyramid (fake, protocol test body, real body) is not actually realizable as described because "protocol test body" has no concrete specification and may not faithfully stand in for real input/timing.
- **Evidence**: Session 14 says "How faithfully the protocol test body stands in for the real body: measured after 2.7 and the first level-2 tests" (§left open). Session 2.7 defines E1-E14 for eyes/hands/takeover but says "Whether a protocol test body stands in for the real body" is risk #4 in architecture (§11). Also 2.5 says the same port serves fake/real/replay, but the *input path* (SendInput/hardware timing/focus) is what only level 3 tests. The design relies on level 2 to do "verification, receipts, the practice room" but admits it doesn't know if it stands in.
- **Recommendation**: Do not claim level 2 covers input timing/focus/takeover. Explicitly classify what each level can/cannot test in a matrix with pass/fail criteria (e.g. "level 2 can test receipt logic but NOT take-over latency"). Treat "protocol test body fidelity" as a gate experiment before committing to level 2 as the workhorse. Also consider that many admission tests (session 11 practice room) require world copy on test server - that's more like level 2 infrastructure than a pure protocol model.
- **Confidence**: high

#### TESTING-2
- **ID**: TESTING-2
- **Severity**: blocker
- **Where**: `docs/sessions/02-body-connection.md` §2.7, `docs/architecture.md` §11 risk #1, #3
- **Claim**: The critical experiments (E1-E14) have hard pass/fail thresholds but no mitigation plan if Azalea/forwarder/input fails, and the design says "Claude never switches route on its own" - yet for a one-person project this creates a potential dead-end with no fallback timeline.
- **Evidence**: "A threshold that is missed stops the work and goes to the user; Claude never switches route (for example to a client mod) on its own" (2.7). Architecture lists risks: forwarder hop breaks 5ms budget (E1-3), Azalea may not decode 26.3 or be slow (E2, E3; fallback MCProtocolLib), synthetic input may not reach game at endgame timing (E5-9). But no decision on *when* to try MCProtocolLib vs own decoder vs client mod fallback; client mod is mentioned as "fallback" in session 1/2 but ruled out as preferred route and there's no concrete plan.
- **Recommendation**: For each risky experiment define (a) binary pass/fail, (b) concrete fallback action (try X, scope reduction Y) that requires user approval but has a pre-agreed shortlist, (c) timebox. Also note "round 1 on macOS first" for eyes but hands/focus *must* be at physical Windows PC (RDP blocks raw input) - one-person project needs access to physical Windows machine at right time; this is a logistics blocker not just technical.
- **Confidence**: high

#### TESTING-3
- **ID**: TESTING-3
- **Severity**: major
- **Where**: `docs/sessions/14-testing.md` §"Evidence standard", `docs/decisions.md` (2026-10-23 success measures), `docs/architecture.md` §9
- **Claim**: "Many fresh seeds, confidence intervals, analysis written before the runs" is underspecified for a one-person project - no concrete statistical plan, no stopping rules, no way to know how many seeds are needed.
- **Evidence**: "Each ladder rung reports reach rate, time and deaths over fresh seeds with intervals (the first-night demo gate: at least 18 of 20 fresh seeds without death). The demo video is one of the measured runs; a hand-picked best run is labelled as such" (14). "Target numbers are set after the first baseline" (14). But no power analysis, no alpha, no handling of variance (worldgen), no definition of "fresh seed" (same MC version/settings?), no plan for multiple comparison across rungs/skills. Also "many" is vague.
- **Recommendation**: Pick a minimal concrete plan now: for binary success (survive first night) use binomial CI (e.g. Wilson score) and pre-commit to N>=30 or use sequential testing with stopping rule; for continuous (time) report median + bootstrap CI. Write analysis template before first baseline (hypotheses, metrics, stopping rule). Otherwise "analysis written before runs" is performative.
- **Confidence**: high

#### TESTING-4
- **ID**: TESTING-4
- **Severity**: major
- **Where**: `docs/sessions/04-action-verification.md`, `docs/sessions/07-reflexes-arbiter.md`, `docs/architecture.md` §5
- **Claim**: The verification model ("server's word", receipts, `unconfirmed`/`unverifiable`) is strong in principle but the design conflates "verifiable by policy" with "measurable reliably" and gives no concrete false-positive budget for `confirmed`.
- **Evidence**: "An action succeeds only with the server's word (level 2 for world changes, level 3 when an effect is expected). No confirmation gives `unconfirmed`, then re-read the truth, then retry" (arch §5). "false `confirmed` must be zero (survey/01 idea 3)" (session 4). Session 7 says "a referee reads server facts and Praxion's own report never decides a verdict". But how to construct referee fixtures for all cases? Fault injection listed as backlog item ("false `confirmed` must be 0") but no method specified. Also "unverifiable tasks... never counted as successes" is correct but could be gamed by over-marking things unverifiable.
- **Recommendation**: Treat "false confirmed == 0" as a hard invariant testable by fault injection (drop acks, reorder packets, inject stale inventory) with explicit fixtures. Define "server's word" precisely per action class (which packet/state change counts). Add a metric "% actions marked unverifiable" to detect gaming/over-conservatism. Make referee able to detect mismatches automatically.
- **Confidence**: high

#### TESTING-5
- **ID**: TESTING-5
- **Severity**: major
- **Where**: `docs/sessions/11-skill-learning.md`, `docs/sessions/14-testing.md`, `backlog.md` (session 11 gate tests)
- **Claim**: Admission pipeline (static checks -> practice room 3 runs vs baseline -> probation -> active) is elaborate but "practice room on a test server" is expensive and the fidelity of test server vs real world is unmeasured; also learning test at equal compute is specified but no baseline.
- **Evidence**: "Never first tried in the user's world" (11). Backlog says "measure fixture restore time (above 30 s means a nightly batch)". Session 14 says level 2 is "headless client on a local test server" for "verification, receipts, the practice room". But worldgen differences, tick timing, plugin/mod absence, loaded chunks, latency to test server all differ from real play. Also "skill learned in Praxion's own play becomes active without asking" but gated same way - safety is good, but testing cost could kill iteration for one person.
- **Recommendation**: Measure fixture restore time and test-server fidelity early (part of 2.7/level 2 bringup). Consider fake-world (level 1) gate as stronger filter where possible, with level 2 only for actions that need server confirmation. Also pre-register the "equal compute" learning test (control vs treatment) before running it to avoid HARKing.
- **Confidence**: high

#### TESTING-6
- **ID**: TESTING-6
- **Severity**: major
- **Where**: `docs/sessions/08-jev-llm-brain.md`, `docs/decisions.md` (2026-10-29 Jev decision), `backlog.md` session 8
- **Claim**: Jev is "wired in by measurement" but measurement plan is vague and "latency sweeps" won't capture real decision quality or cost; also "never cut automatically" on cost brake is nice but makes cost untestable as a constraint.
- **Evidence**: Jev only on options code lists, deadline >1s, compared with rules and LLM at each decision point. Backlog says "p50/p95/p99 latency of Jev and candidate LLMs from user's PC; does cancelling a call stop billing; do APIs stream structured JSON while reasoning; is Jev calibrated with 5 to 80 options..." Latency sweeps with injected delays proposed. But "value is unproven" (arch risk #5). Also "cost brake: track and warn above user-set threshold; never cut automatically" means you cannot test "stays under cost X" as a hard requirement.
- **Recommendation**: Define Jev success as measurable win over rules+LLM on specific decision classes (accuracy/correctness with receipts, not just speed) using held-out scenarios. Test billing behavior (cancellation) explicitly - some providers don't refund. Also decide if Jev is required for demo or optional; if optional and unproven, don't block demo on it.
- **Confidence**: high

#### TESTING-7
- **ID**: TESTING-7
- **Severity**: major
- **Where**: `docs/sessions/14-testing.md`, `docs/sessions/13-natural-feel.md`, `docs/decisions.md`
- **Claim**: "Naturalness (human movement statistics and a blind test)" is in success measures but untestable without a proper baseline and method - risk of gaming or vague pass/fail.
- **Evidence**: Session 13 says "Movement style is a general human style, not fitted to the user. The recording of the user's play serves as the evaluation reference; personalising stays a later option." Session 14 lists "movement statistics against held-out human play; blind test with OBS recordings" in inventory. But no metrics defined (path curvature, look acceleration, key press duration distributions), no sample size, no inter-rater agreement, no way to avoid experimenter bias in blind test.
- **Recommendation**: Pre-define 2-3 concrete distributions (e.g. inter-key intervals, camera angular velocity) with tolerances from held-out user recordings. For blind test, use proper randomization and "spot the bot" with controls; maybe start with Turing-test style but be realistic about sample size for one person.
- **Confidence**: medium

#### TESTING-8
- **ID**: TESTING-8
- **Severity**: minor
- **Where**: `docs/architecture.md` §9, `docs/sessions/14-testing.md`
- **Claim**: "Fast tests on every change" conflicts with reality - some tests need game state/setup and "every change" will be expensive; also cost tracking mentioned but no CI budget.
- **Evidence**: "Fast tests run on every change; levels 2 and 3 run nightly or on request" (arch). But even level 1 (fake world) might be slow if complex; level 2 needs test server. For one person on Windows dedicated PC, "every change" might mean every commit locally - need to be selective (unit tests vs integration). Also tokens/cost recorded but no limits.
- **Recommendation**: Define "fast" explicitly (e.g. <60s, no network) and split test suites (unit/integration/e2e). Don't require level 2 on every change.
- **Confidence**: high

#### TESTING-9
- **ID**: TESTING-9
- **Severity**: minor
- **Where**: `docs/sessions/14-testing.md` §"LLM reasoning", `docs/sessions/08-jev-llm-brain.md`
- **Claim**: Logging says "hosted models show summarised reasoning only, so the log keeps the structured directive and its stamp (session 8), not a raw trace" - this hurts debugging and reproducibility.
- **Evidence**: "LLM reasoning: hosted models show summarised reasoning only, so the log keeps the structured directive and its stamp (session 8), not a raw trace" (14 minor picks). But for debugging skill failures/LLM mistakes you often need reasoning. Also "every claim cites a receipt id" but LLM decisions may not produce receipts in same form.
- **Recommendation**: Log the full prompt/response structure (without raw chain-of-thought if provider doesn't give it), tool calls, schema validation results, and any reasoning summary provided. Even if reasoning hidden, structured data helps.
- **Confidence**: medium

#### TESTING-10
- **ID**: TESTING-10
- **Severity**: minor
- **Where**: `docs/sessions/04-action-verification.md`, `docs/architecture.md` §5
- **Claim**: "Unverifiable tasks... never counted as successes" is good for honesty but can be abused to make metrics look better; also definition is circular.
- **Evidence**: "A task whose success cannot be computed within the perception policy is `unverifiable` and never counted as success" (arch). "unverifiable, world-unsuitable and precondition do not [count as failure]" (session 4). But who decides it's unverifiable? Need clear criteria. Also could mark hard tasks as unverifiable to avoid counting failures.
- **Recommendation**: Make unverifiability decision auditable (log reason), have referee validate unverifiability decisions in sample, track unverifiability rate separately.
- **Confidence**: medium


### Ideas (brainstorm)

1. **Test the proxy first with packet capture** - Before writing Azalea decoder, record a real offline session and write a reference parser for just the packets you need. This makes E2 testable even if Azalea branch is flaky, and gives you a ground truth for level 2.
2. **Fake world with scripted server responses** - Instead of full protocol test body, have level 1.5: fake world that can inject packet drops/acks/stale state to test receipts/fault injection cheaply. This directly tests "false confirmed == 0" without needing test server.
3. **Property-based testing for guards** - Since compiler derives guards from primitive effects, use property tests (e.g. quickcheck/proptest) to verify guards are necessary/sufficient against random world states. Catches effect declaration bugs (backlog session 5).
4. **Measurement-driven Jev gate** - Don't wire Jev until you have a pre-registered A/B test showing it wins on >=3 decision classes with confidence intervals. This avoids building infrastructure for unproven component.
5. **"Replay the death" fixture factory** - Every death becomes a fixture automatically (session 7 says this). Automate extraction from event log + server state to make reflex coverage matrix grow without manual work.
6. **Chaos monkey for brain** - Test "brain silent -> safe stop" (decision 2.5) with random process kills, network drops. Cheap to do and critical for safety.
7. **Deterministic seeds for CI** - Use fixed seeds for regression tests, fresh seeds only for statistical evaluation. Prevents flaky tests while still doing proper measurement.


### Questions for the user

1. **Physical access**: Do you have reliable access to the dedicated Windows PC for rounds 2-4 of 2.7 experiments (hands/takeover/focus)? RDP won't work per docs - is this machine always available when needed?
2. **Test server setup**: Who maintains the local vanilla server for level 2 tests? Is it acceptable to run headless server on same machine as game, or does it need separate setup?
3. **Jev budget**: Is there any cost limit for Jev/LLM during testing/evaluation, or is "track and warn, never cut automatically" the only policy? This affects how many seeds/runs are feasible.
4. **Wiki RAG license**: The design says index stays local and undistributed due to CC BY-NC-SA, but licence conflict with open-core model is open (arch §8). Do you want to proceed with wiki RAG despite potential licensing issues, or find alternative?
5. **Demo gate priority**: The first demo is "hand-over, verified chat command, instant take-over, autonomous first night" (18/20 fresh seeds). Is this demo gate blocking for moving to next phase, or is it aspirational while you do 2.7 experiments?

FINAL_REPORT The testing design is ambitious but has three critical gaps: (1) the "protocol test body" fidelity is unknown and untested, yet it's meant to carry most tests - treat its validity as a gate experiment before relying on it; (2) the 2.7 experiments have hard pass/fail thresholds with no agreed fallback plan and require physical Windows access at the right time, creating high risk of deadlock for a one-person project; (3) the statistical rigor ("many seeds, confidence intervals, analysis before runs") is underspecified - without concrete power analysis and stopping rules, "measured" could become "cherry-picked". Fix these three before committing to the full pyramid.
