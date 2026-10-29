<!-- Reviewer: Beam Free via OpenCode opencode v2.1.2. Lens: beam-premortem. Raw, unverified output. -->

### Findings

**PM-1 — blocker**
- **Where**: `docs/architecture.md` §2, §4 (Decoder); `docs/sessions/02-body-connection.md` §2.2, §2.7 E2
- **Claim**: The decoder the whole perception layer rests on does not actually support the target version today; Azalea's `main` advertises Minecraft 26.2, and the 26.3 branch is unverified and may never merge.
- **Evidence**: Live GitHub page for `azalea-rs/azalea` states "Currently supported Minecraft version: 26.2"; the repo docs only assert a `26.3` branch last pushed 2026-10-16 (unverified by us). MCProtocolLib, by contrast, already merged 26.3 on 2026-10-14.
- **Recommendation**: Run experiment E2 against the 26.3 branch before any product code; if it fails, switch the primary decoder to MCProtocolLib (accepting the Java/Rust boundary) rather than waiting on upstream.
- **Confidence**: high

**PM-2 — blocker**
- **Where**: `docs/sessions/02-body-connection.md` §2.3, §2.7 E5–E12; `docs/architecture.md` §11 risk 1
- **Claim**: The central bet — synthetic OS input reaching Minecraft's raw-input path with endgame precision — is entirely untested, and failure would force the client-mod fallback, contradicting "unmodified client" and "avoid mods" and invalidating most of the design.
- **Evidence**: Experiments E5–E12 are not run; MC-230466 documents relative-input quirks; GLFW behaviour under SendInput is inferred from community reports, not documented.
- **Recommendation**: Run rounds 2–4 on the physical Windows PC now, with a hard go/no-go gate before product code; pre-decide the fallback scope so a failure does not strand the project.
- **Confidence**: high

**PM-3 — major**
- **Where**: `docs/architecture.md` §3, §7; `docs/sessions/02-body-connection.md` §2.5; `docs/sessions/08-jev-llm-brain.md`
- **Claim**: Hosted-model dependence is a single point of failure with no real degraded mode: a silent brain makes the body "stop safely", so any outage or latency spike halts autonomous progress for hours, and Jev is early-access with no SLA.
- **Evidence**: TypeSafe site describes Jev as "early access"; decisions say silence → safe stop; cost brake "warns, never cuts"; no hard budget.
- **Recommendation**: Define a degraded-but-playing mode (run last stamped directive + rules with a cached subgoal) instead of stopping; add a hard per-run/per-hour budget that pauses autonomous play (not reflexes) and asks the user; benchmark at least one alternative middle-tier model.
- **Confidence**: high

**PM-4 — major**
- **Where**: `docs/architecture.md` §1; `docs/decisions.md` 1.5/1.6; `backlog.md`
- **Claim**: Scope and capacity are mismatched: one person, zero product code, unrun experiments, yet the target is the whole game to the Ender Dragon with a "flawless" live demo to a community and professor audience.
- **Evidence**: 15 sessions decided, no code; experiments E1–E14 unrun; demo gate "at least 18 of 20 fresh seeds without death" plus a live hand-over.
- **Recommendation**: Re-scope the first shippable milestone to "first night survived on ≥18/20 seeds"; move rungs 2–6 to a roadmap; set an explicit code-start gate.
- **Confidence**: high

**PM-5 — major**
- **Where**: `docs/sessions/03-perception-world-model.md`; `docs/sessions/10-autonomy.md` (fair-play stronghold finding); `docs/architecture.md` §5
- **Claim**: The fair-play perception policy (line of sight, no hidden data) is a self-imposed handicap that may make endgame rungs (stronghold location, dragon phase) infeasible or force fragile heuristics, while competing agents use omniscient server state.
- **Evidence**: Session 3 denies mob health and dragon phase counter; session 10 proposes Bayesian eye-of-ender flight estimation.
- **Recommendation**: Run an early ablation measuring rung reach with and without hidden-data access; decide whether fair-play is a hard constraint or a measured trade-off before committing to it.
- **Confidence**: medium

**PM-6 — major**
- **Where**: `docs/decisions.md` 2026-10-29 (free curriculum); `docs/sessions/10-autonomy.md`; `backlog.md` (curriculum comparison arm)
- **Claim**: The free Voyager-style curriculum was chosen against the recommendation (ladder spine), risking unverifiable goals and wandering that keeps the agent off the next rung.
- **Evidence**: Decision records the user's choice over the recommended spine; backlog asks to log goals that keep Praxion away from the next rung.
- **Recommendation**: Keep the comparison arm in the learning test; enforce in code that every proposed goal carries a computable completion condition and that the curriculum prompt includes rung status; allow switching to ladder-spine mode.
- **Confidence**: medium

**PM-7 — major**
- **Where**: `docs/sessions/11-skill-learning.md`; `backlog.md` (gate tests, fixture restore time)
- **Claim**: The skill-admission gate is slow (k=3 on a test-server world copy; fixture restore >30 s means nightly only) and research shows most learned skills are used once, so the "writes its own skills" claim may be hollow.
- **Evidence**: Session 11 stages: practice room on a separate test server; backlog flags restore time >30 s; survey notes most learned skills used once and verified programs help.
- **Recommendation**: Measure fixture restore time early; if >30 s, use the in-memory fake world for the first gate; define an honest metric (skills reused ≥N times) and accept a null result.
- **Confidence**: medium

**PM-8 — major**
- **Where**: `docs/sessions/01-product-scope.md` §1.1; `docs/sessions/02-body-connection.md` (version baseline); `docs/architecture.md` §10
- **Claim**: Targeting only the newest stable release (26.3) with broad version support "later" leaves the design exposed to ~2-month release cadence: protocol, data generator, GUI layout and `options.txt` keys all change, and the "version pack" boundary is untested.
- **Evidence**: Session 1 notes every release changes protocol and breaks bots; session 2 lists 26.3 as target with 26.4 snapshots already in flight.
- **Recommendation**: Pin 26.3 for the demo; implement the version-pack boundary in first code; add a version-bump checklist and regenerate data packs per release.
- **Confidence**: medium

**PM-9 — major**
- **Where**: `docs/sessions/14-testing.md`; `docs/sessions/02-body-connection.md` §2.7
- **Claim**: Windows-only, physical-PC level-3 testing cannot run in CI and depends on one person's machine, so real-input bugs are found late, at demo time.
- **Evidence**: Session 14 levels 2/3 run nightly or on request on the dedicated PC; E5–E12 require the physical machine.
- **Recommendation**: Maximize level-1/2 coverage; record seeds and replays; document repro steps; consider a second cheap Windows box for level-2.
- **Confidence**: medium

**PM-10 — major**
- **Where**: `docs/sessions/08-jev-llm-brain.md` (cost brake); `docs/architecture.md` §3
- **Claim**: "Warn, never cut" plus event-driven LLM calls (60 s heartbeat + events) is a policy, not a safeguard; a bug causing repeated calls can explode cost with no automatic stop.
- **Evidence**: Cost brake explicitly "never cut automatically"; every call class has a deadline and fallback but no budget cap.
- **Recommendation**: Add a hard per-run and per-hour budget that pauses autonomous play (never reflexes) and prompts the user; keep warn-only for demo mode.
- **Confidence**: medium

**PM-11 — minor**
- **Where**: `docs/sessions/02-body-connection.md` §2.5; `docs/architecture.md` §10–§11
- **Claim**: The body/brain message format and transport are "chosen at first code", an unspecified integration contract between Rust and Python that invites rework.
- **Evidence**: Session 2.5 leaves message format and transport open; architecture §12 lists "message formats and syntax" as not in draft.
- **Recommendation**: Define a minimal versioned schema (JSON over TCP/UDS or a length-prefixed binary) before coding; keep it behind the body port.
- **Confidence**: medium

**PM-12 — minor**
- **Where**: `docs/sessions/02-body-connection.md` §2.2; `docs/architecture.md` §4
- **Claim**: Azalea is a one-person hobby project with breaking changes and Bevy-ECS coupling; Praxion's decoder and (planned) planner extraction depend on it, risking breakage or stall.
- **Evidence**: GitHub shows 803 stars, single maintainer, "Many parts unfinished, breaking changes"; README flags breaking changes in changelog.
- **Recommendation**: Depend only on protocol/buf crates; keep an own world model; write own A* following the cost model; retain MCProtocolLib as fallback.
- **Confidence**: high

**PM-13 — minor**
- **Where**: `docs/sessions/02-body-connection.md` §2.3 (E10); `docs/architecture.md` §4 (Hands)
- **Claim**: Menu slot clicking depends on GUI scale, window size and version-specific layout; E10 tests 4 scales but not all versions, so inventory/crafting automation can silently mis-click.
- **Evidence**: H2 uses absolute positioning from GUI scale and window size; E10 pass ≥99/100 across scales 1–4 and two window sizes.
- **Recommendation**: Treat the container-click packet's slot index as ground truth and verify every click by packet; re-calibrate on mismatch.
- **Confidence**: medium

**PM-14 — minor**
- **Where**: `docs/decisions.md` 1.5/1.6; `docs/sessions/14-testing.md`
- **Claim**: A "flawless" live demo with a community and professor audience creates overfitting pressure; any single live failure is catastrophic and Ender Dragon scope is not demo-realistic.
- **Evidence**: Demo scenario includes hand-over, verified command, take-over, first night; capability target is Ender Dragon; 1.6 says the demo video is a measured run and hand-picked best runs are labelled.
- **Recommendation**: Make the live demo only hand-over + first night; keep the Ender Dragon off the live demo; present pre-recorded measured runs labelled as such.
- **Confidence**: medium

### Ideas (brainstorm)

- Replace the Azalea dependency with a thin own decoder over Mojang data-generator reports, using Azalea only as a reference — removes upstream risk and Bevy coupling.
- Make the first shippable milestone "first night survived on ≥18/20 seeds" and treat rungs 2–6 as roadmap — kills the scope-vs-capacity risk.
- Add a degraded-but-playing brain mode that runs the last stamped directive plus rules on silence instead of stopping — keeps autonomy during transient outages.
- Use the in-memory fake world as the first skill-admission gate, promoting to the test server only for final — ~10x faster iteration.
- Record every run as a replay and make the demo a replayed measured run — removes live-demo risk.
- Make Jev optional and swappable, benchmarking a small open classifier as the default middle tier — avoids single-vendor lock-in.
- Expose perception profiles as a toggle for ablation — settles whether fair-play is a hard constraint or a measured trade-off.

### Questions for the user

1. Is the fair-play perception policy a hard constraint, or a measured trade-off? (Affects endgame feasibility.)
2. Should the first shippable milestone be re-scoped to first night only, deferring the Ender Dragon?
3. If Azalea 26.3 fails experiment E2, is the client-mod fallback acceptable despite "avoid mods"?
4. Should there be a hard cost budget that pauses autonomous play (never reflexes)?
5. Is Jev's early-access risk acceptable, or should an open model be the default middle tier?

FINAL_REPORT The top failure risks are an unverified decoder dependency on an Azalea branch that still only supports Minecraft 26.2, an untested synthetic-input path whose failure would force a client-mod fallback that contradicts the project's own constraints, and a one-person team with no product code yet targeting a flawless live demo up to the Ender Dragon; together these threaten the project before any code ships, and each has a cheap now-mitigation (run E2 and the Windows input experiments immediately, re-scope the first milestone to first night, and define a degraded-but-playing brain mode plus a hard cost budget).
