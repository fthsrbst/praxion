<!-- Reviewer: GLM-5.3-Flash via OpenCode. Draft-2 review, lens: glm-consistency. Raw output. -->

The three review docs are in. Before drafting findings, I need to verify a few suspected mis-tags against the session notes (the draft's [D] tags claim sources in `decisions.md`, but some claims may only be session minor-picks).
Two suspects resolved (the 3 s outage figure is indeed a session-2.5 pick, and "preempted" is a decided session-2.5/4 outcome). Let me check the demo-script wording in session 1 and a few remaining tag suspects.
## Findings

1. **Severity: major · Where: architecture.md §6.3 (Storage bullet) · Claim: “The skill writer is a low-privilege process that can write only to a candidates directory [D].”**
   **Evidence:** `decisions.md` has no such bullet. The claim exists only as a session-15 minor pick: `sessions/15-infrastructure.md` line 32, under the heading "Minor picks by Claude (user may overrule)": *"Process isolation: the skill writer (session 11) runs as a separate low-privilege process; the exact Windows mechanism is chosen when it is built."* — explicitly overrulable and the mechanism left open. The phrase "candidates directory" appears nowhere else in the repo. §4 line 145 repeats it untagged, reinforcing decided status.
   **Fix:** Change `[D]` to `[P, session-15 minor pick]` in §6.3 and tag the §4 Skill-writer row clause the same way. Confidence: high.

2. **Severity: minor · Where: architecture.md §9 (Ideas [P]) · Claim: the stock-client capture corpus is an un-agreed idea.**
   **Evidence:** §9: "Ideas [P]: a stock-client capture corpus with a differential decode oracle; a death-to-fixture factory; …". But the decided 2.7 amendment (`decisions.md` 2026-10-29) states the round-0 desk step includes "a stock-client capture corpus", and §11 risk 2 already treats it as fact ("round 0 corpus, E2, E3"). An item inside the accepted experiment plan is listed under a [P] tag that means "proposed, not yet agreed".
   **Fix:** Remove the corpus from the "Ideas [P]" list (or tag it `[D, round 0]`); keep the oracle agreement method there as [P]. The death-to-fixture factory can stay [P]. Confidence: high.

3. **Severity: minor · Where: architecture.md §6.2 (precondition line) · Claim: the primitive-contract precondition list matches the decided one.**
   **Evidence:** Decided wording (`decisions.md` 2026-10-29, primitive contract): "a hand-written precondition block (reach, support, free space, light, line of sight)". Architecture §6.2: "preconditions: hand-written (in reach, in sight, supported, safe to expose)" — "free space" and "light" are gone, "safe to expose" (which round 1, R1-M10, placed in the *planner* contract, not primitive preconditions) is substituted.
   **Fix:** Write "(in reach, supported, free space, light, in sight)"; leave `safe_to_expose` in §5.5 where R1-M10 put it. Confidence: high.

4. **Severity: minor · Where: architecture.md §5.2 (Take-over [D]) and §5.5 (Input [D]) · Claim: decided text carries clauses that no decision records.**
   **Evidence:** §5.2: "danger-held keys stay held until the user touches them or the moment passes, and no tier can revoke them [D-block]" — the second clause is an R1-M5 proposal; the decided safe-take-over exception (`decisions.md`, session 4) says only "its needed keys stay held until the user touches them or the moment passes". §5.5: "the planning state (known blocks through the perception API, hotbar, best tool, protected blocks)" — the decided planner bullet lists "hotbar, best tool and protected blocks"; "known blocks through the perception API" is an addition.
   **Fix:** Tag "no tier can revoke them" and "known blocks through the perception API" as `[P]` clauses, or drop them. Confidence: high.

5. **Severity: minor · Where: architecture.md §4 (components table) · Claim: untagged rows read as decided, but correspond to [P] content verified in their sections.**
   **Evidence:** (a) Watchdog row: "own process; releases keys and buttons if the body dies" — untagged, while §2 tags the same behaviour "[P, R1-M5]". (b) Hands row is untagged except the nonce, and Take-over row lists "danger-held keys released last" untagged, though that too is R1-M5 (proposed). (c) Obtain-X solver row (§4) is untagged while §6 tags its output properties "[P]" — a reader cannot tell whether the solver (a session-9/10 pick absent from `decisions.md`) is decided. (d) Version manifest row: "pins client, server jar, protocol 777, Azalea commit, toolchain date, data-report hash, dependency licences; the forwarder refuses a handshake with another protocol number [P]" — the single trailing [P] reads as scoping to the last clause, leaving "data-report hash, dependency licences" (critic-7 additions, un-decided) seeming decided.
   **Fix:** Tag each §4 row's un-decided clauses inline: Watchdog "[P]", "released last [P]", obtain-X "[P, session 9/10 pick]", and add a comma so [P] visibly covers data-report hash and licences. Confidence: high.

6. **Severity: minor · Where: architecture.md Appendix A, row "Brain on the same PC; one repository; keys in the credential store; Apache-2.0 | §2, §10, §7, §11" · Claim: Apache-2.0 is traceable to §11.**
   **Evidence:** §11 never mentions Apache-2.0; risk 13 covers only "which parts are commercial". The licence exists in `decisions.md` (session 15) but the architecture never states it anywhere.
   **Fix:** Either add "Apache-2.0 open core" to §11 risk 13 (e.g., "Apache-2.0 open core; which parts are commercial: business-model decision") or change the section column to note "licence stated in `decisions.md`, not in §11". Confidence: high.

7. **Severity: minor · Where: architecture.md §1 (Out of scope: "local models") vs §11 risk 15 · Claim: "local models" are out of scope, while a local component is implied.**
   **Evidence:** §1: "Out of scope for now [D]: … local models …". §11 risk 15 lists "embedder" among co-loading processes; session 12 decided a local embedding model ("Qwen3-Embedding (Apache-2.0, local, free)"). The decided out-of-scope rule ("Models are hosted, not local. Reason: decision quality") is about decision models, but the text does not say so, so a reader may read the local embedder as violating the [D] rule — or read "the only network dependence" (§2) as unproven.
   **Fix:** §1: "local decision models (Jev/LLM replacements)"; the embedder is then consistent. Confidence: medium.

**Areas checked with no findings:** all 22 `decisions.md` 2026-10-29 bullets are reflected in draft 2 (body port v0, event log, intent stack, Kleene guards, receipts/`unrefuted`, planner contract, version pin 26.3, forwarder topology and its fail path, physical-operation-only, ControlFrame, command gesture, command box, safety without the brain, dedicated-server trade-offs, cost brake, admission evidence path, world backup, raw-store exclusivity, languages); the previously flagged Jev-mapping mis-tag ([D]→[P], R1-M12) was correctly fixed; the "3 s" brain-silence figure and its [P] tag are backed by `sessions/02` line 228; "preempted" is a decided outcome (sessions 2.5/4); §6.1 Kleene truth table, §5.4 generation rules, §5.6 control verbs, §3 cost brake and §6.3 admission statistics match the decided wording; Appendix A's "Gaps: none found" holds — every decided bullet is represented; the arithmetic (1.1 M cells for 4/3·π·64³; Wilson lower bound 0.44 at 3/3, n=8) checks out.

### Verdict

Draft 2 is ready to drive the 2.7 experiments, starting with desk round 0. Every decision the experiments stand on — the round-0 content, the version pin, the forwarder topology, the Windows-only latency judging, E15/E16 and the fallback-shortlist stop rule — is recorded and correctly worded, and the five blockers of round 1 are all closed as decided. The findings above are tag hygiene and wording precision, not design gaps: the one major (the low-privilege skill-writer presented as [D]) is a cheap retag that should be fixed before the draft is treated as a map of what the user has agreed, but nothing found changes what round 0 or any experiment measures. Fix the seven items in a small edit pass, then proceed.

FINAL_REPORT: 0 blockers, 1 major, 6 minors
