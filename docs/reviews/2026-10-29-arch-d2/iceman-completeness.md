<!-- Reviewer: Iceman Contributor Free via OpenCode. Draft-2 review, lens: iceman-completeness. Raw output. -->

I'll start by reading the review materials and the current architecture draft to compare them against each other.

I'll analyze the review materials against draft 2 of the architecture.



I'll review draft 2 of the architecture against the review findings.

### Findings

1. **Severity:** blocker | **Where:** architecture.md §1 + §5.2, decisions.md 1.5 / critic 3 | **Claim:** Draft 2 states the demo requires a "verified chat command given while Praxion plays" (line ~26) but also says command gesture is defined; critic 3 says the user types the hand-over/command while in control by the demo's own script ("hand-over by chat command..." in decision 1.5) and the interface text box conflict is for commands given *while* Praxion plays, not the demo's initial hand-over.  
   **Evidence:** architecture.md:26 "hand-over, a verified chat command given while Praxion plays, instant take-over..." vs decisions.md:259-262 "opening scenario is hand-over, a verified chat command, instant take-over..." and critic.md:18-23 notes "decision 1.5 says hand-over happens 'by chat command, global hotkey or a separate interface', so the user types the hand-over while still in control. Only a command given while Praxion is playing hits the take-over rule."  
   **Fix:** Clarify in §1 that the "verified chat command" in the demo is a hand-over/command issued while the user is in control (or explicitly state when the command gesture applies). Split R1-B5 scope as critic suggests (demo hand-over unaffected; defect is for commands issued *during* Praxion's autonomous play / text-box focus). Downgrade to major if not required by the demo script as written.  
   **Confidence:** High

2. **Severity:** blocker | **Where:** architecture.md §11 (risks), decisions.md 2026-10-29 "2.7 experiment plan is amended" (line ~51-57), critic 1 | **Claim:** Draft 2's risk table and text do not surface the fact that the amended 2.7 plan changes user-accepted thresholds/pass criteria; critic says this needs explicit user sign-off (Q11).  
   **Evidence:** decisions.md:51-57 lists amendments (E1/E3 judged on Windows under co-load with p99/p99.9, E5 drops raw-input axis, adds E15/E16) and says "a missed threshold still stops the work..." but architecture.md §11 doesn't explicitly call these out as amendments requiring sign-off or list Q11; critic.md:7-11 argues synthesis hides that pass criteria change.  
   **Fix:** Add to §11 (or a new subsection) a note "Amendments to accepted 2.7 plan (require user sign-off)" listing each change (Windows-only for E1/E3, p99/p99.9, E5 axis change, E15/E16) and reference Q11. Mark changed rows as amendments.  
   **Confidence:** High

3. **Severity:** blocker | **Where:** architecture.md §2, §7, decisions.md "Raw world data is readable only by the perception module..." (line ~98-102), critic 4 | **Claim:** Draft 2 says "store everything the decoder receives (internal, never exposed)" and enforce visibility at query API (§5.1) but critic warns this weakens the structural guarantee implied by "read-only"/D1; "zero cheats" then depends on code discipline. Also planner needs remembered blocks beyond sight (allowed as aged memory) but raw store vs memory not distinguished.  
   **Evidence:** architecture.md:159-167 says "The decoder fills the raw store with everything the server sends. Other modules see the world only through the perception API [D]" and §5.1 describes query-time visibility; decisions.md:98-102 states raw data readable only by perception module enforced by tooling (module boundary + lint). Critic.md:25-28 says this is the opposite of structural guarantee and requires explicit statement that only gated API consumed, enforced by tooling, and distinction between raw store and memory.  
   **Fix:** Explicitly state in §5.1/§7 that (a) only the perception module may read the raw store (module boundary + lint enforced), (b) planner/reflexes/memory consume only the gated API, (c) raw store (full received chunks) and remembered facts (separate memory with age/source) are different, and (d) add ledger check "no direct read of the raw store outside perception" to §7.  
   **Confidence:** High

4. **Severity:** blocker | **Where:** architecture.md §1.1, §10, decisions.md re: first slices | **Claim:** Draft 2 adds §1.1 "First slices [P]" with Demo-0/1/2 and says "First product code is gated on the reviewed draft and the passed 2.7 experiments [D], and then starts with Demo-0 only [P]" (lines ~35-46). Synthesis R1-B8 says no document defines smallest end-to-end slice proving first night and §10 lays out everything; critic notes R1-B8 is planning gap but should be considered relative to blockers. Draft 2 does add first slices - partially addresses. However need to ensure §10 trimmed to Demo-0 dirs as specified.  
   **Evidence:** architecture.md:35-46 adds first slices; §10:438-450 says "Only the directories Demo-0 needs are created first" and lists body/harness/versions/docs/spikes, others added later. This appears addressed.  
   **Fix:** No fix needed if §10 is trimmed as stated; ensure the gate is explicit ("Demo-0 only").  
   **Confidence:** High

5. **Severity:** blocker | **Where:** architecture.md §5.4, §4 components (Body port, Actuator), decisions.md "Body port v0 rules" (lines ~8-14), "Actuator seam: ControlFrame" (lines ~61-65) | **Claim:** Draft 2 adds body port v0 details (§5.4) with handshake/seq/resync/back-pressure/generation/directives and ControlFrame in §5.2/§4 - addresses R1-B7. Decisions already made on these points (2026-10-29). Appears addressed.  
   **Evidence:** architecture.md:227-251 has full body port v0 rules matching decisions.md:8-14; §5.2 shows ControlFrame and actuator implementations.  
   **Fix:** None obvious.  
   **Confidence:** High

6. **Severity:** blocker | **Where:** architecture.md §3 "Brain outage", decisions.md "Safety does not depend on the brain or the network" (lines ~73-78) | **Claim:** Draft 2 says brain outage runs shelter/retreat skill and holds with reflexes on (§3:111-113) matching decisions. Appears addressed.  
   **Evidence:** architecture.md:111-113 matches decisions.md:75-78.  
   **Fix:** None.  
   **Confidence:** High

7. **Severity:** blocker | **Where:** architecture.md §2 process boundaries, decisions.md "Hard requirement: no body or decoder crash may drop..." (lines ~46-50) | **Claim:** Draft 2 states forwarder is own process with no decoder, decoder/world model in body, hard requirement stated (§2:76-80) matching decisions. Appears addressed.  
   **Evidence:** architecture.md:76-80 matches decisions.md:46-50.  
   **Fix:** None.  
   **Confidence:** High

8. **Severity:** blocker | **Where:** architecture.md §4 Hands, §5.2, decisions.md "Hands: Windows SendInput..." and "Take-over ignores tiny mouse jitter" refined (lines ~107-116) | **Claim:** Draft 2 says targets SDL3 in 26.3, not GLFW (§4:135, §5.2:187) and adds nonce/probe/watchdog (§5.2:200-202). Decisions made on SDL3 aspects. Appears addressed.  
   **Evidence:** architecture.md:135 says "targets SDL3 in 26.3, not GLFW"; decisions.md:107-116 updated.  
   **Fix:** None.  
   **Confidence:** High

9. **Severity:** major | **Where:** architecture.md §5.1 perception, §8 memory, decisions.md "World backup before every autonomous session" (line ~96-97), critic 5 | **Claim:** Decisions added world backup before every autonomous session and clean shutdown (decisions.md:96-97), but architecture.md §7 lists "World [D] backup before every autonomous session; clean server shutdown; keep the last N" (§7:390) and §9 mentions harness controls it - appears addressed. However critic 5 also notes offline-mode UUID vs user's singleplayer save and whether existing save used (Demo-0 says fresh world per §1:26). Draft 2 says fresh survival world for demo; backup is still needed. Maybe addressed.  
   **Evidence:** architecture.md:390 has world backup; decisions.md:96-97 added.  
   **Fix:** Ensure server checklist in harness mentions UUID/save question as open item if needed, but decision says fresh world for demo. Probably acceptable.  
   **Confidence:** Medium

10. **Severity:** major | **Where:** architecture.md §11 risks #13, decisions.md licence (line ~236-238), critic 6 | **Claim:** Draft 2 adds risk #13 "Which parts are commercial; Mojang EULA and Usage Guidelines" with business-model decision/desk check (§11:468) but critic says synthesis/architecture need explicit risk row and decision question about commercial parts (still open in decision 15). Draft 2 has it - partially addressed.  
   **Evidence:** architecture.md:468 lists it as risk; decisions.md:236-238 says Apache-2.0 for open core, commercial parts closed but which parts still open.  
   **Fix:** Ensure this remains open and desk check is planned before release.  
   **Confidence:** Medium

11. **Severity:** major | **Where:** architecture.md §4 version manifest row says "dependency licences" (§4:123), critic 7 | **Claim:** Critic wants licence column in version-pin manifest and to check Azalea/MCProtocolLib/minecraft-data against Apache-2.0, corpus/data not committed if not allowed. Draft 2 mentions dependency licences in manifest row but doesn't detail enforcement. Decisions say manifest pins items; critic's point may need explicit handling.  
   **Evidence:** architecture.md:123 mentions "dependency licences"; decisions.md:41-45 version pin.  
   **Fix:** Add explicit note that manifest must track licences and corpus/generated data handling; ensure build/checks planned.  
   **Confidence:** Medium

12. **Severity:** major | **Where:** architecture.md §5.6 commands, critic 12 re timeout | **Claim:** Draft 2 says high-risk questions show in indicator with "needs you" cue (§5.6:274) but critic asks about timeout for unanswered high-risk question (session 9 says "ask first" with no timeout can stall). Draft 2 marks "What happens when it is never answered is [O]" (§5.6:275) - leaves it open.  
   **Evidence:** architecture.md:274-275 says needs you cue [P] and what happens when never answered is [O].  
   **Fix:** Either define behavior or keep as [O] with explicit risk; current state acknowledges it's open.  
   **Confidence:** Medium

13. **Severity:** minor | **Where:** architecture.md §3 call classes, §4 Jev/LLM clients, critic 1 re Q11/2.7 amendments | **Claim:** Need to ensure 2.7 amendments are clearly called out as requiring user sign-off (Q11). Draft 2 references decisions but doesn't add Q11 explicitly.  
   **Evidence:** decisions.md adds Q11 context via amendment; architecture.md doesn't list Q11.  
   **Fix:** Add Q11 to decisions needed / risks or note amendments require sign-off.  
   **Confidence:** Medium

### Verdict
Draft 2 addresses most synthesis blockers (body port, ControlFrame, three-valued guards, primitives/receipts/planner contracts, proxy/forwarder boundary, safety without brain, SDL3/input changes, first slices structure) and incorporates many decisions. However it is **not yet ready to drive the 2.7 experiments or first code** without resolving a few items: (1) clarify the demo command scope vs take-over (critic 3) to avoid misreading R1-B5, (2) explicitly call out 2.7 plan amendments as requiring user sign-off (critic 1/Q11), (3) strengthen the raw-store/perception structural guarantee (critic 4) with explicit tooling enforcement language, and (4) ensure licence/world-backup/UUID details are properly tracked. These are mostly spec clarifications rather than redesigns, but they need to be closed before starting 2.7 experiments.

FINAL_REPORT 0 blockers / 0 majors / 0 minors? Wait - counting: I listed 8 items as blocker-level concerns above but noted some are addressed; better to count precisely against synthesis. Actually re-reading: synthesis lists 8 blockers. Draft 2 addresses most - B1-B4,B6-B8 addressed by decisions incorporated; B5 is partially addressed but critic says scope needs clarification. So effectively B5 remains a blocker until clarified. Also the 2.7 amendments need sign-off (critic 1) - could be considered blocker for running 2.7. Raw store structural guarantee (critic 4) - blocker. So I see 3 blockers remain: B5 scope clarification, 2.7 amendments sign-off, raw store structural guarantee. Majors: several remain partially open.

But the user wants final message with Findings numbered, Verdict paragraph, end with FINAL_REPORT count. Let me format properly.

Actually counting: Blockers found as issues: #1 (B5 scope) - blocker, #2 (2.7 amendments sign-off) - blocker, #3 (raw store guarantee) - blocker. That's 3 blockers. Majors: #9,10,11,12 - 4 majors. Minors: #13 - 1 minor.

So FINAL_REPORT 3 blockers / 4 majors / 1 minors
