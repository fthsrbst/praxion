<!-- Reviewer: Beam Free via OpenCode opencode v2.1.2. Lens: beam-ux-feel. Raw, unverified output. -->

### Findings

- **ID**: NF-1  
  **Severity**: major  
  **Where**: `docs/architecture.md` §4 (Interface window [O]), §2, §7; `docs/sessions/02-body-connection.md` §2.4  
  **Claim**: The UX-critical surface (reports, command box, live view, memory viewer) is entirely undecided in technology and interaction model, while the "windows never take focus" invariant makes its text input unusable as specified.  
  **Evidence**: architecture.md §4 row "Interface window | not decided [O]"; decisions 2.4 "Praxion's own windows never take focus"; §9 "reports go to the separate interface first"; §9 "text from the interface".  
  **Recommendation**: Decide the interaction model before any code: either in-game chat is the sole text input and the interface is read-only/overlay, or the interface may take focus on an explicit user action (click or global hotkey) and release it on blur. Document which.  
  **Confidence**: high

- **ID**: NF-2  
  **Severity**: major  
  **Where**: `docs/sessions/09-commands.md` "Ambiguous commands"; `docs/architecture.md` §5 "Commands"  
  **Claim**: For low-risk ambiguous commands Praxion guesses, starts working and asks at the same time, so it can spend minutes on a misinterpreted goal before the user notices.  
  **Evidence**: session 09 "Low risk: Praxion guesses, starts working and asks at the same time ('collecting iron, is that what you meant?'); if the user answers differently it switches."  
  **Recommendation**: Add a visible pending-question state in the indicator with a clear cancel/switch control, and make side-effecting guesses require a one-keystroke confirm when a cheaper safe default exists.  
  **Confidence**: medium

- **ID**: NF-3  
  **Severity**: major  
  **Where**: `docs/decisions.md` 2026-10-29 "After a take-over, the interrupted task waits"; `docs/architecture.md` §5 "Hand-over and take-over"  
  **Claim**: Any momentary user adjustment (e.g. a camera nudge) forces an explicit "carry on" command; otherwise Praxion stands idle indefinitely.  
  **Evidence**: "the interrupted task waits for 'carry on'"; "It never resumes from its own memory of the state."  
  **Recommendation**: Offer an optional auto-resume after a configurable idle period (e.g. 5–10 s of no physical input) with a visible countdown and one-key cancel; keep strict wait-for-command mode for risky contexts.  
  **Confidence**: medium

- **ID**: NF-4  
  **Severity**: major  
  **Where**: `docs/sessions/02-body-connection.md` §2.4 "Minor picks"  
  **Claim**: Hand-over requires no held keys and waits up to ~2 s then fails, which is worst exactly when the user wants Praxion to take over quickly.  
  **Evidence**: "Hand-over starts only when no physical key or button is held (wait up to ~2 s, then tell the user)."  
  **Recommendation**: Queue hand-over and start the instant all keys release, with a "release to hand over" prompt; do not require re-pressing the hotkey.  
  **Confidence**: high

- **ID**: NF-5  
  **Severity**: major  
  **Where**: `docs/decisions.md` 2026-10-29 "Praxion is active indicator"; `docs/sessions/02-body-connection.md` §2.4  
  **Claim**: On a single-monitor dedicated PC, "a window outside the game area" has no defined placement and will overlap either the game or the taskbar.  
  **Evidence**: "a small separate window outside the game area (second monitor if present)".  
  **Recommendation**: Define a single-monitor fallback (e.g. a thin non-activating strip at a screen edge, or a taskbar/notification-area indicator) and state which.  
  **Confidence**: high

- **ID**: NF-6  
  **Severity**: major  
  **Where**: `docs/sessions/09-commands.md` "Reports"; `docs/decisions.md` 2026-10-29 "Praxion's own windows never take focus"  
  **Claim**: Because reports and high-risk questions go to the interface first and windows never take focus, with no specified sound/flash/notification, the user can miss them and stall or be surprised.  
  **Evidence**: "Reports go to the separate interface first"; "Praxion's own windows never take focus."  
  **Recommendation**: Specify an attention mechanism (system notification sound, taskbar flash, and a "needs you" badge in the indicator) and define which events use it.  
  **Confidence**: high

- **ID**: NF-7  
  **Severity**: minor  
  **Where**: `docs/sessions/09-commands.md` "Facts that shape the design"; `docs/architecture.md` §5 "Commands"  
  **Claim**: In-game chat acknowledgements stop movement for 1–3 s and appear as the user's own character speaking, which is disruptive and odd in recordings.  
  **Evidence**: "Praxion can only write to in-game chat by typing: the movement stops for about 1 to 3 s, and in player-bound mode the message appears as the user's own character speaking."  
  **Recommendation**: Minimize in-game chat output; batch acknowledgements; default to interface-only reporting and make in-game chat opt-in per event type.  
  **Confidence**: high

- **ID**: NF-8  
  **Severity**: minor  
  **Where**: `docs/sessions/10-autonomy.md` "Decided"; `docs/architecture.md` §5 (autonomy)  
  **Claim**: Big risky steps proceed after a ~30 s window unless the user says stop, so an absent user can have Praxion enter the Nether/End or fight the dragon unattended.  
  **Evidence**: "Big risky steps are announced ... 'preparing for the Nether, going in about 30 s'; if the user says stop it stops, otherwise it goes."  
  **Recommendation**: Make the window configurable, require an explicit first-time confirm per risk category, and offer an "always ask" mode.  
  **Confidence**: medium

- **ID**: NF-9  
  **Severity**: minor  
  **Where**: `docs/sessions/13-natural-feel.md` "Decided"; `docs/decisions.md` 2026-10-29 "Human limits without fatigue"  
  **Claim**: "No faked breaks" and no fatigue means long autonomous runs may look unnaturally tireless, a cue the blind naturalness test could catch.  
  **Evidence**: "Praxion does not tire or fake breaks (it sleeps only when the game needs it)."  
  **Recommendation**: Keep the decision but add subtle idle/observation behaviors (looking around, pausing to plan) as part of the movement/decision layers, measured against the blind test.  
  **Confidence**: low

- **ID**: NF-10  
  **Severity**: minor  
  **Where**: `docs/sessions/09-commands.md` "Ambiguous commands"; `docs/architecture.md` §5 "Commands"  
  **Claim**: High-risk "ask first and wait" has no timeout or escalation, so a missed question halts progress indefinitely.  
  **Evidence**: session 09 "High risk: it asks first and waits."  
  **Recommendation**: Add a configurable timeout that aborts the step safely, re-asks later, and marks the indicator with a "waiting for you" badge.  
  **Confidence**: medium

- **ID**: NF-11  
  **Severity**: minor  
  **Where**: `docs/architecture.md` §9 "Testing and observability"  
  **Claim**: A "live view showing what Praxion is thinking" risks dumping raw LLM reasoning, creating noise and privacy exposure rather than useful transparency.  
  **Evidence**: "A live window and a replayable event log show what Praxion is thinking."  
  **Recommendation**: Filter the live view to decisions, actions, receipts and questions; keep chain-of-thought in the local event log only, with an explicit toggle.  
  **Confidence**: medium

- **ID**: NF-12  
  **Severity**: minor  
  **Where**: `docs/sessions/09-commands.md` "Decided"; `docs/architecture.md` §5 "Commands"  
  **Claim**: Typing prefixed commands in-game while playing is error-prone and interrupts play; the interface command box is better but is blocked by the focus conflict (NF-1).  
  **Evidence**: "Command channels: in-game chat with a prefix, and the separate interface ... text box."  
  **Recommendation**: Prioritize the interface command input with a global hotkey, autocomplete and history; keep the chat prefix as a fallback.  
  **Confidence**: high

### Ideas (brainstorm)

- **Peek-and-resume**: let the user grab control for a few seconds and auto-resume with a visible countdown, instead of forcing "carry on".  
  Why: removes the biggest daily friction in hand-over/take-over without weakening safety.
- **"Needs you" attention layer**: sound + taskbar flash + indicator badge for questions and reports, with quiet hours.  
  Why: the interface never takes focus, so attention must be earned explicitly.
- **Command palette in the interface**: global hotkey, fuzzy search over verbs, argument completion, history.  
  Why: typing `p: collect wood` in chat mid-fight is hostile UX.
- **Ambient status strip**: health, hunger, current task, ETA and next planned action, expandable.  
  Why: the indicator currently shows only who plays, task and last command.
- **One-click run replay**: export a session as video/GIF with receipts overlaid.  
  Why: the blind naturalness test and community demos both need shareable artifacts.
- **Persona presets**: calm/terse/chatty tone and a renameable tag, selectable in settings.  
  Why: "short, calm, informative" is a default, not a universal preference.
- **"Show me" command**: Praxion points the camera at a named thing for a few seconds.  
  Why: builds trust and makes the agent feel like a companion, not a black box.

### Questions for the user

1. Should the interface window be allowed to take focus on an explicit user action, despite "windows never take focus"?  
2. After a take-over, should Praxion auto-resume after a short idle period, or always wait for "carry on"?  
3. Where should the indicator live on a single-monitor setup?  
4. What attention modality (sound, taskbar flash, in-game ping) should questions and reports use?  
5. Should in-game chat output be minimal by default (interface-first), or verbose for streamers?

FINAL_REPORT: The three most important findings are that the interface window — the sole surface for reports, commands, the memory viewer and the live view — is entirely undecided in technology and interaction model while the "windows never take focus" invariant makes its command box unusable as specified (NF-1); that after any take-over the task waits indefinitely for an explicit "carry on" even for a one-second camera nudge (NF-3); and that hand-over stalls up to ~2 s when keys are held, failing precisely in urgent moments (NF-4). Together these mean the hand-over/take-over loop that the first demo depends on is specified in a way that will feel sticky and frustrating rather than instant and natural, and the interaction model must be settled before implementation.
