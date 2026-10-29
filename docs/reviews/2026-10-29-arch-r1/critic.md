<!-- Completeness critic: Claude Sonnet 5.5 (workflow subagent), 2026-10-29, on synthesis.md. -->

## Critic: gaps and corrections

I checked the synthesis against `docs/decisions.md`, `docs/sessions/02-body-connection.md` and `docs/sessions/09-commands.md`, and against a grep of the raw reviews. I did not re-read all 18 raw files in full, so the "silently dropped" items below are spot findings, not an exhaustive audit.

1. **The 2.7 changes overwrite user-accepted thresholds, and the synthesis does not flag it.**
   - What: `decisions.md` says "Thresholds accepted as listed... a missed threshold stops the work." The synthesis then changes E1/E3 to be judged only on Windows, adds p99.9, drops the "raw input on/off" axis from E5, and adds E15 and E16. It says "stop rule unchanged", which hides that the pass criteria themselves change.
   - Why it matters: these are edits to a decision the user made on 2026-10-29. Under AGENTS.md, "Consult before deciding", they need the user's sign-off.
   - Fix: add one question, Q11, "Amend the accepted 2.7 plan: E1/E3 judged on Windows only, p99.9 reported, E5 without the raw axis, new E15/E16". Mark every changed row in the 2.7 section as an amendment to a decided item.
   - Evidence: `sessions/02-body-connection.md` §2.7, "Decided (user, 2026-10-29)" and the experiment table. The same section says "Round 1 (also on macOS)" and "Round 1 first: it is cheap and decides the decoder". R1-B4 and "round 0" change that order.

2. **R1-B4 misses a reason the stricter hardware requirement is already settled.**
   - What: the synthesis never uses the decided sentence "Remote Desktop blocks raw relative input" in Q8 (remote play).
   - Fix: cite it in Q8 as decided evidence. That makes the "operated only physically" recommendation near-decided rather than an open choice.
   - Evidence: `sessions/02-body-connection.md` §2.7 ("Hands experiments run at the physical Windows PC"), plus the decision "The PC is dedicated to the game while Praxion plays".

3. **R1-B5 may partly misread the demo, and its fix needs an accurate premise.**
   - What: decision 1.5 says hand-over happens "by chat command, global hotkey or a separate interface", so the user types the hand-over while still in control. Only a command given while Praxion is playing hits the take-over rule.
   - Evidence for that case: session 9 says "stop and cancel always work" and "a new command suspends the old one". The interface is "the window of 2.4 with a text box" (`sessions/09-commands.md` line 15). That same window is specified as `WS_EX_NOACTIVATE`. So the conflict is real for the text box, but the synthesis overstates it for the demo.
   - Why it matters: R1-B5 is rated a blocker for the "first demo", and its Q1/Q2 framing could prompt a redesign that is not needed.
   - Fix: split R1-B5. (a) Demo hand-over is unaffected, because the user is in control when typing. (b) Commands during Praxion's play, and the text-box focus conflict, are the real defect. Downgrade it to major unless the demo script (1.5) literally requires a command while Praxion plays; the synthesis should say which reading it used.
   - Unverified: I did not find the exact wording of the demo script beyond decisions 1.5 and 2.4.

4. **R1-M3 weakens a structural guarantee, and the synthesis does not say so.**
   - What: "Store everything the decoder receives (internal), enforce visibility at the query API." Session 3 and the decision "line of sight... a wall hides what is behind it" were meant as a policy, but the body now holds an x-ray of the whole received chunk set. "Zero cheats" then depends on code discipline, which is the opposite of how D1 treats read-only ("holds structurally").
   - Fix: say this explicitly. Require that the planner, reflexes and memory consume only the gated API, enforced by a module boundary and tooling (decision 15: "layers are enforced by tooling"). Add the ledger check "no direct read of the raw store outside `perception`" to §7.
   - Also: the planner needs remembered blocks beyond the sight limit, which session 3 allows as aged memory. Say that the raw store and the memory are not the same thing.

5. **World data-loss is not covered.** This is a review angle nobody took.
   - What: Praxion plays the user's character in a world, can die ("death is acceptable"), digs and bridges anywhere, and runs on a dedicated server that the user's client logs into in offline mode.
   - Gaps:
     - There is no snapshot or backup policy before a session. A bad skill, a stuck-key bug or a ruleset hole can damage the world.
     - The server must be stopped cleanly to flush; the checklist should say so.
     - R1-M6 says offline-mode UUIDs "matter only for imported worlds". That is understated: with `online-mode=false` the user's character gets a different UUID, so the "user's own character" is a different player data file from the user's singleplayer one. I could not verify what vanilla does with a singleplayer `Player` tag in `level.dat` on first server load. Treat it as an open item for the checklist.
   - Fix: add to the server checklist a mandatory world copy (zip of the world folder) before each autonomous session, a world-version marker and a clean-shutdown rule. Name "does the user's existing singleplayer save get used?" as a question. Decision 1.5 only says "fresh survival world", so this may be out of scope for Demo-0.
   - Evidence: `grep` over `docs/` found no hit for backup, UUID or save import in decisions or sessions. One raw review (a Beam premortem) recommends managing the server as a child process "(EULA, properties, save copy, bind 127.0.0.1)", and the synthesis keeps only the EULA part. I saw this in the grep output; I did not trace which cluster it fell into.

6. **Legal and terms angle is missing: EULA, Usage Guidelines and the open-core licence.**
   - What: decision 15 says "Apache-2.0 for the open core, commercial parts closed." The synthesis handles only the Minecraft Wiki's CC BY-NC-SA (C32). Nobody checked Mojang's EULA and Usage Guidelines for a commercial product that automates play, or for distributing recordings or tooling that mention "Minecraft".
   - What I could verify: the EULA says the Usage Guidelines are extra permissions that Mojang can change or withdraw at any time, that 3rd-party tools must not seem "official", and that Mojang may take down Mods or software that violate the Usage Guidelines. Mojang's 2017 changes banned brand-promo mods and servers.
   - What I could not verify: the fetch of the EULA and Usage Guidelines pages timed out, so I have no first-hand text on automation or bots, or on commercial products that use the game name. The 2017 coverage is old.
   - Fix: add a risk row, "Mojang EULA and Usage Guidelines versus the planned commercial parts; no 'official' look; name use". Add a desk task before any public release. The decision to ask the user: which parts are commercial (still open in decision 15).
   - Sources: https://www.minecraft.net/sv-se/eula and https://onmsft.com/news/minecraft-cracks-branded-promotions-servers-mods/ (via a web search; neither page text could be re-read here).

7. **Dependency licences and the Mojang data reports are unchecked against Apache-2.0.**
   - What: the synthesis recommends Azalea, MCProtocolLib and `minecraft-data` as decoder, oracle and backup. Session 2 itself notes `minecraft-data`'s "license unstated in its README".
   - Also: the "capture corpus" (idea 1) holds stock-client traffic and, with Mojang's game jar, Mojang's content. If the corpus or generated reports are committed to the open repository, that may be redistribution.
   - Fix: add a licence column to the version-pin manifest. State that the corpus and generated data are not committed (or committed only if allowed). I did not check the Azalea or MCProtocolLib licences in this pass.

8. **Severity ranking questions.**
   - R1-B8 (no first slice) is a planning gap, not a correctness blocker for 2.7. It is fine to keep as "blocks first code" but should not sit in the same tier as R1-B1 and R1-B2, which block the experiments. Add a column "blocks: 2.7 / first code / neither".
   - R1-B6 (safety without the brain) mixes three things: a verb parser, the indicator source, and an outage behaviour that amends a decision (Q5). Only the first two are cheap and conflict-free. Split so the user can approve those without being drawn into Q5.
   - R1-M12 (traceability) is rated major, but the list it gives is exactly what a reader needs to trust the whole review. Move it into the blockers, or at least do it first: it is cheap and every other fix depends on it.

9. **Claims in the synthesis that no doc or source backs.**
   - "SDL main special-cases `hDevice == NULL`... injected `SendInput` events carry `hDevice = 0`": the synthesis itself says "reported, unverified". It is still the basis of a blocker. Keep it as hypothesis, and rate the blocker on "the analysis targets a different input stack", not on this detail.
   - "`white-list` reportedly defaults to true in 26.3 (Nodecraft; verify)": single secondary source. Fine as written, but do not put it in a checklist as fact.
   - Cluster numbers ("about 1.1 M cells") and the "McNemar k=3 gives p=0.25" are computed by the adjudicator, not cited. They are plausible, but add the arithmetic in one line so the user can check them, since the user "must understand every part".
   - The external facts (26.3 ships SDL, `rawMouseInput` gone, Azalea README says 26.2) were verified by the orchestrator. I did not re-verify them.

10. **Review angle: onboarding the user to understand every part.**
    - What: AGENTS.md says the user must understand every algorithm and interface, and agents "never design alone". The synthesis adds roughly 30 new mechanisms (nonce tagging, Kleene guards, deny-by-default ray budgets, epoch rules, actuator frame, event-log projections, E15/E16) and asks 10 questions. It carries no plan for how the user absorbs them, and it never says which additions the user has not yet seen.
    - Fix: before draft 2, group the proposed additions into 4 to 5 teaching sessions in dependency order (input stack; port and actuator; perception and memory; skills and admission; safety and brain outage). Mark each addition as "needs your decision", "needs your understanding", or "backlog". Apply "No silent decisions" to the minor picks the adjudicator made (for example R1-M3 "rejected: Beam's filter" and the dropped items), by listing them for the user to overrule.

11. **Maintenance burden and scope growth are not weighed.**
    - What: Engineering principle 2 says "Question every addition". The synthesis adds E15, E16, the actuator frame, the version-pin manifest, a doctor, the death-to-fixture factory, shadow mode and the Jev replay harness. It never costs them, and nothing says what to cut.
    - Fix: tag each "Ideas worth adopting" item with cost (S/M/L) and the demo it first serves. The ideas are suggestions, but several (shadow mode, death-to-fixture factory) are phase-2 work and should go to `backlog.md`, not the 2.7 plan. Idea 7 (shadow mode) also needs a check against "the user's own play is recorded locally now; learning later" and the privacy of recording the user's input.

12. **Dropped items to double check against the raw files.**
    - C35 (auto-resume after take-over) is dropped as contradicting decision 2.4. That is right, but the same cluster's ask, "bounded wait for carry on", and the cluster "Ambiguous commands lack pending state, attention and timeout" appear to be only partly kept (a "needs you" cue for high-risk questions). Check that a timeout for an unanswered high-risk question is covered; session 9's "ask first" with no timeout can stall the character forever.
    - Cluster "No auditory perception channel" maps to hearing in §8. Session 3 picks hearing, so the fix is only to add it to the document, as the synthesis says. No change needed.
    - Cluster "LLM cannot patch primitive quirks" appears only as "an optional suspected-primitive field" inside R1-M8. If that cluster was major, one field is a thin answer: say how a primitive bug is ever reported to a human.

Files read: `/Users/fatih/praxion/docs/decisions.md`, `/Users/fatih/praxion/docs/sessions/02-body-connection.md` (lines 100–300), `/Users/fatih/praxion/docs/sessions/09-commands.md` (grep), `/Users/fatih/praxion/docs/reviews/2026-10-29-arch-r1/merged-clusters.json` (titles only), `/Users/fatih/praxion/docs/reviews/2026-10-29-arch-r1/verdicts.json` (structure only), and the raw directory (grep only).
